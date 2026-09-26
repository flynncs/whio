use super::*;
use crate::features::music_library::storage::contracts::{CatalogueWrite, LibraryRead, StateStore};
use crate::features::music_library::{
    domain::{AlbumMetadata, ArtistCredit, ProviderId, SourceIdentity, SourceScope, TrackMetadata},
    storage::postgres::PostgresTrackRepository,
};

fn local_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named("/music".to_owned()),
        external_id.to_owned(),
    )
    .unwrap()
}

fn metadata(title: &str) -> TrackMetadata {
    TrackMetadata::from_names(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

async fn service(
    pool: sqlx::PgPool,
) -> (LibraryStateService, Arc<PostgresTrackRepository>, TrackId) {
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let track = repository
        .get_or_create(local_source("path-a"), metadata("Song"))
        .await
        .unwrap();
    let service = LibraryStateService::new(
        Arc::clone(&repository) as Arc<dyn StateStore>,
        Arc::clone(&repository) as Arc<dyn LibraryRead>,
    );

    (service, repository, track.id().clone())
}

#[sqlx::test]
async fn starring_rating_and_playing_are_per_user(pool: sqlx::PgPool) {
    let (service, repository, track_id) = service(pool.clone()).await;
    let user = crate::features::identity::provision(pool.clone())
        .create_user("state-test", "State Test")
        .await
        .unwrap();

    service
        .set_starred(user, StateTarget::Track(&track_id), true)
        .await
        .unwrap();
    assert!(
        service
            .rate_id(user, &track_id.to_string(), Some(4))
            .await
            .unwrap()
    );
    service
        .record_play(user, &track_id, chrono::Utc::now())
        .await
        .unwrap();
    service
        .record_play(user, &track_id, chrono::Utc::now())
        .await
        .unwrap();

    let state = repository.find_track_state(&track_id, user).await.unwrap();
    assert!(state.is_starred());
    assert_eq!(state.rating(), Some(4));
    assert_eq!(state.play_count(), 2);

    let starred = service.starred(user).await.unwrap();
    assert_eq!(starred.tracks().len(), 1);
    assert!(starred.albums().is_empty());
    assert!(starred.artists().is_empty());
    assert!(
        service
            .starred(Uuid::now_v7())
            .await
            .unwrap()
            .tracks()
            .is_empty()
    );
}

#[sqlx::test]
async fn unknown_targets_are_skipped(pool: sqlx::PgPool) {
    let (service, _repository, _track_id) = service(pool.clone()).await;
    let user = crate::features::identity::provision(pool.clone())
        .create_user("state-test", "State Test")
        .await
        .unwrap();

    service
        .set_starred(user, StateTarget::Track(&TrackId::new()), true)
        .await
        .unwrap();
    service
        .record_play(user, &TrackId::new(), chrono::Utc::now())
        .await
        .unwrap();
    assert!(
        !service
            .rate_id(user, &AlbumId::new().to_string(), Some(3))
            .await
            .unwrap()
    );
    assert!(service.starred(user).await.unwrap().tracks().is_empty());
}

#[sqlx::test]
async fn ratings_resolve_albums_and_artists(pool: sqlx::PgPool) {
    let (service, repository, _track_id) = service(pool.clone()).await;
    let user = crate::features::identity::provision(pool.clone())
        .create_user("state-test", "State Test")
        .await
        .unwrap();
    let artist = repository
        .resolve_or_create_artist(&ArtistCredit::new("Artist".to_owned(), None).unwrap())
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(&AlbumMetadata::new("Album".to_owned()).unwrap())
        .await
        .unwrap();

    assert!(
        service
            .rate_id(user, &album.id().to_string(), Some(5))
            .await
            .unwrap()
    );
    assert!(
        service
            .rate_id(user, &artist.id().to_string(), Some(2))
            .await
            .unwrap()
    );
    assert!(!service.rate_id(user, "not-an-id", Some(1)).await.unwrap());

    let summary = repository
        .find_album_summary(album.id(), Some(user))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(summary.state().rating(), Some(5));
}

#[tokio::test]
async fn a_disabled_service_holds_no_state() {
    let service = LibraryStateService::disabled();
    let user = Uuid::now_v7();

    assert!(service.starred(user).await.unwrap().tracks().is_empty());
    service
        .set_starred(user, StateTarget::Track(&TrackId::new()), true)
        .await
        .unwrap();
    assert!(!service.rate_id(user, "not-an-id", Some(1)).await.unwrap());
}
