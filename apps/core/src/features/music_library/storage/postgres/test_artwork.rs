use chrono::{Duration, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use super::PostgresTrackRepository;
use crate::features::music_library::{
    artwork::{ArtworkAssetDescriptor, ArtworkRole, ArtworkVariant},
    domain::ArtistId,
    storage::contracts::{ArtworkStore, TrackRepositoryError},
};

fn digest(variant: ArtworkVariant, revision: i64) -> String {
    format!("{:0>64}", format!("{}-{revision}", variant.as_str()))
}

fn asset(
    subject: &ArtistId,
    role: ArtworkRole,
    revision: i64,
    variant: ArtworkVariant,
) -> ArtworkAssetDescriptor {
    ArtworkAssetDescriptor {
        id: Uuid::now_v7(),
        subject: subject.clone(),
        role,
        source_provider_id: "youtube_music".to_owned(),
        source_external_id: "channel-1".to_owned(),
        revision,
        variant,
        content_digest: digest(variant, revision),
        content_type: "image/jpeg".to_owned(),
        width: 256,
        height: 256,
        byte_size: 1024,
        file_name: format!("{}.jpg", Uuid::now_v7()),
    }
}

#[sqlx::test]
async fn find_ready_is_scoped_to_the_subject_and_role(pool: PgPool) {
    let store = PostgresTrackRepository::new(pool.clone());
    let subject = ArtistId::new();
    let other = ArtistId::new();
    let first = asset(&subject, ArtworkRole::Portrait, 1, ArtworkVariant::Original);
    let latest = asset(&subject, ArtworkRole::Portrait, 2, ArtworkVariant::Original);
    let latest_derived = asset(&subject, ArtworkRole::Portrait, 2, ArtworkVariant::Px256);
    let wide = asset(&subject, ArtworkRole::Wide, 1, ArtworkVariant::Original);
    let elsewhere = asset(&other, ArtworkRole::Portrait, 1, ArtworkVariant::Original);

    store
        .record_assets(&[
            first,
            latest.clone(),
            latest_derived.clone(),
            wide.clone(),
            elsewhere.clone(),
        ])
        .await
        .unwrap();

    let ready = store
        .find_ready(&subject, ArtworkRole::Portrait)
        .await
        .unwrap();

    assert_eq!(ready.len(), 3);
    assert!(ready.iter().all(|row| row.subject == subject));
    assert!(ready.iter().all(|row| row.role == ArtworkRole::Portrait));
    assert_eq!(ready.first().map(|row| row.revision), Some(2));
    assert!(ready.iter().any(|row| row.id == latest.id));
    assert!(ready.iter().any(|row| row.id == latest_derived.id));

    let ready_wide = store.find_ready(&subject, ArtworkRole::Wide).await.unwrap();

    assert_eq!(ready_wide.len(), 1);
    assert_eq!(ready_wide[0].id, wide.id);
    assert_eq!(ready_wide[0].variant, ArtworkVariant::Original);

    let ready_other = store
        .find_ready(&other, ArtworkRole::Portrait)
        .await
        .unwrap();

    assert_eq!(ready_other.len(), 1);
    assert_eq!(ready_other[0].id, elsewhere.id);
}

#[sqlx::test]
async fn record_assets_writes_the_set_once_and_ignores_a_replayed_set(pool: PgPool) {
    let store = PostgresTrackRepository::new(pool.clone());
    let subject = ArtistId::new();
    let assets = vec![
        asset(&subject, ArtworkRole::Portrait, 1, ArtworkVariant::Original),
        asset(&subject, ArtworkRole::Portrait, 1, ArtworkVariant::Px256),
        asset(&subject, ArtworkRole::Portrait, 1, ArtworkVariant::Px768),
    ];

    store.record_assets(&assets).await.unwrap();
    store.record_assets(&assets).await.unwrap();

    let mut ready = store
        .find_ready(&subject, ArtworkRole::Portrait)
        .await
        .unwrap();
    ready.sort_by_key(|row| row.content_digest.clone());
    let mut expected = assets.clone();
    expected.sort_by_key(|row| row.content_digest.clone());

    assert_eq!(ready.len(), 3);
    assert_eq!(
        ready
            .iter()
            .map(|row| row.content_digest.clone())
            .collect::<Vec<_>>(),
        expected
            .iter()
            .map(|row| row.content_digest.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        ready.iter().map(|row| row.id).collect::<Vec<_>>(),
        expected.iter().map(|row| row.id).collect::<Vec<_>>()
    );
}

#[sqlx::test]
async fn a_row_that_violates_a_column_check_is_rejected(pool: PgPool) {
    let store = PostgresTrackRepository::new(pool.clone());
    let subject = ArtistId::new();
    let invalid = asset(&subject, ArtworkRole::Portrait, 1, ArtworkVariant::Original);
    let invalid = ArtworkAssetDescriptor {
        content_digest: "not-a-digest".to_owned(),
        ..invalid
    };

    let outcome = store.record_assets(&[invalid]).await;

    assert!(matches!(outcome, Err(TrackRepositoryError::InvalidData)));
    assert!(
        store
            .find_ready(&subject, ArtworkRole::Portrait)
            .await
            .unwrap()
            .is_empty()
    );
}

#[sqlx::test]
async fn lru_descriptors_order_by_last_use_and_honour_exclude_and_limit(pool: PgPool) {
    let store = PostgresTrackRepository::new(pool.clone());
    let subject = ArtistId::new();
    let first = asset(&subject, ArtworkRole::Portrait, 1, ArtworkVariant::Original);
    let second = asset(&subject, ArtworkRole::Portrait, 1, ArtworkVariant::Px256);
    let third = asset(&subject, ArtworkRole::Portrait, 1, ArtworkVariant::Px768);

    store
        .record_assets(&[first.clone(), second.clone(), third.clone()])
        .await
        .unwrap();

    let base = Utc::now();
    store.touch(third.id, base).await.unwrap();
    store
        .touch(second.id, base + Duration::seconds(1))
        .await
        .unwrap();
    store
        .touch(first.id, base + Duration::seconds(2))
        .await
        .unwrap();

    let ordered = store.lru_descriptors(None, 10).await.unwrap();

    assert_eq!(
        ordered.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![third.id, second.id, first.id]
    );

    let excluded = store.lru_descriptors(Some(second.id), 10).await.unwrap();

    assert_eq!(
        excluded.iter().map(|row| row.id).collect::<Vec<_>>(),
        vec![third.id, first.id]
    );

    let limited = store.lru_descriptors(None, 2).await.unwrap();

    assert_eq!(limited.len(), 2);
    assert_eq!(limited[0].id, third.id);
    assert_eq!(limited[1].id, second.id);
}

#[sqlx::test]
async fn delete_asset_reports_whether_the_row_existed(pool: PgPool) {
    let store = PostgresTrackRepository::new(pool.clone());
    let subject = ArtistId::new();
    let row = asset(&subject, ArtworkRole::Portrait, 1, ArtworkVariant::Original);
    let id = row.id;

    store.record_assets(&[row]).await.unwrap();

    assert!(store.delete_asset(id).await.unwrap());
    assert!(!store.delete_asset(id).await.unwrap());
    assert!(
        store
            .find_ready(&subject, ArtworkRole::Portrait)
            .await
            .unwrap()
            .is_empty()
    );
}
