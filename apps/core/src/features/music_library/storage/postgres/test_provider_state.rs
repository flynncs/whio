use super::PostgresTrackRepository;
use crate::features::music_library::domain::{
    ProviderEntityState, ProviderId, SourceIdentity, SourceScope,
};
use crate::features::music_library::storage::contracts::{CatalogueWrite, SourceLookup};
use sqlx::PgPool;

fn source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn state(reported_count: Option<u32>, last_error: Option<&str>) -> ProviderEntityState {
    ProviderEntityState::new(
        "2026-09-21T00:00:00Z".parse().unwrap(),
        reported_count,
        last_error.map(str::to_owned),
    )
    .unwrap()
}

#[sqlx::test]

async fn provider_entity_state_round_trips_and_upserts(pool: PgPool) {
    let repository = PostgresTrackRepository::new(pool);
    let album = source("MPREb_album");

    assert!(
        repository
            .find_provider_entity_state(&album)
            .await
            .unwrap()
            .is_none()
    );

    let complete = state(Some(12), None);
    repository
        .set_provider_entity_state(&album, &complete)
        .await
        .unwrap();
    assert_eq!(
        repository.find_provider_entity_state(&album).await.unwrap(),
        Some(complete.clone())
    );
    assert!(!complete.is_partial(12));
    assert!(complete.is_partial(1));

    repository
        .set_provider_entity_state(&album, &state(None, Some("provider down")))
        .await
        .unwrap();

    let read = repository
        .find_provider_entity_state(&album)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(read.reported_count(), None);
    assert_eq!(read.last_error(), Some("provider down"));
    assert!(!read.is_partial(1));

    assert!(
        repository
            .find_provider_entity_state(&source("MPREb_other"))
            .await
            .unwrap()
            .is_none(),
        "state is keyed by the provider identity"
    );
}
