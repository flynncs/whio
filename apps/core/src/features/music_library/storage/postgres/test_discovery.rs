use std::sync::Arc;

use sqlx::PgPool;

use super::PostgresTrackRepository;
use crate::features::music_library::{
    domain::{ArtistCredit, ArtistId, ProviderId, SourceIdentity, SourceScope},
    storage::contracts::{CatalogueWrite, SourceLookup},
};

fn source(provider_id: &str, external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new(provider_id.to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

async fn artist(repository: &PostgresTrackRepository, name: &str, mbid: Option<&str>) -> ArtistId {
    repository
        .resolve_or_create_artist(
            &ArtistCredit::new(name.to_owned(), mbid.map(str::to_owned)).unwrap(),
        )
        .await
        .unwrap()
        .id()
        .clone()
}

#[sqlx::test]
async fn find_artist_by_source_returns_the_canonical_id_for_a_known_key(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let known = artist(&repository, "Known", Some("mbid-known")).await;
    repository
        .set_artist_source(&known, &source("musicbrainz", "mbid-known"))
        .await
        .unwrap();

    let found = repository
        .find_artist_by_source(&source("musicbrainz", "mbid-known"))
        .await
        .unwrap();

    assert_eq!(found.map(|artist| artist.id().clone()), Some(known));
}

#[sqlx::test]
async fn find_artist_by_source_returns_none_for_an_unknown_key(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let known = artist(&repository, "Known", Some("mbid-known")).await;
    repository
        .set_artist_source(&known, &source("musicbrainz", "mbid-known"))
        .await
        .unwrap();

    let found = repository
        .find_artist_by_source(&source("musicbrainz", "mbid-absent"))
        .await
        .unwrap();
    let absent = repository
        .find_artist_by_source(&source("youtube_music", "mbid-known"))
        .await
        .unwrap();

    assert_eq!(found, None);
    assert_eq!(absent, None);
}

#[sqlx::test]
async fn find_artist_by_source_distinguishes_the_same_external_id_under_another_scope(
    pool: PgPool,
) {
    let repository = PostgresTrackRepository::new(pool);
    let scoped = artist(&repository, "Scoped", Some("mbid-scoped")).await;
    let global = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Global,
        "same-id".to_owned(),
    )
    .unwrap();
    let named = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        "same-id".to_owned(),
    )
    .unwrap();
    repository
        .set_artist_source(&scoped, &global)
        .await
        .unwrap();

    let found = repository.find_artist_by_source(&named).await.unwrap();

    assert_eq!(found, None);
    assert_eq!(
        repository
            .find_artist_by_source(&global)
            .await
            .unwrap()
            .map(|artist| artist.id().clone()),
        Some(scoped)
    );
}

#[sqlx::test]
async fn find_artist_by_source_uses_the_artist_sources_unique_key(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let first = artist(&repository, "First", Some("mbid-first")).await;
    let second = artist(&repository, "Second", Some("mbid-second")).await;
    let identity = source("musicbrainz", "mbid-shared");
    repository
        .set_artist_source(&first, &identity)
        .await
        .unwrap();
    repository
        .set_artist_source(&second, &identity)
        .await
        .unwrap();

    let found = repository.find_artist_by_source(&identity).await.unwrap();
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;
    let second_sources = sources.find_artist_sources(&second).await.unwrap();

    assert_eq!(found.map(|artist| artist.id().clone()), Some(first));
    assert!(second_sources.is_empty());
}
