use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;

use super::{materialize, materialize_album};
use crate::features::music_library::catalogue::CatalogueCandidate;
use crate::features::music_library::domain::{AlbumTrack, ArtistCredit, TrackMetadata};
use crate::features::music_library::{
    domain::{
        Album, AlbumId, AlbumMetadata, ArtworkRef, ProviderId, SourceIdentity, SourceScope, TrackId,
    },
    storage::contracts::{CatalogueWrite, SourceLookup, TrackRepositoryError},
    storage::postgres::PostgresTrackRepository,
};

fn provider_source(external_id: &str) -> SourceIdentity {
    SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        external_id.to_owned(),
    )
    .unwrap()
}

fn song_candidate(video: &str, duration: u64) -> CatalogueCandidate {
    CatalogueCandidate::new(
        provider_source(video),
        TrackMetadata::from_names(
            "Stop The Wedding!".to_owned(),
            vec!["Ashe".to_owned()],
            Some(duration),
        )
        .unwrap(),
    )
    .with_album(Some(
        AlbumMetadata::new("Album".to_owned())
            .unwrap()
            .with_album_artist(ArtistCredit::new("Ashe".to_owned(), None).unwrap())
            .with_provider(Some(provider_source("MPREb_album"))),
    ))
}

#[sqlx::test]
async fn same_album_provider_songs_share_one_track_and_two_sources(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;
    let hits = materialize(
        &catalogue,
        &sources,
        vec![
            song_candidate("WNxAvQNsJuM", 199_000),
            song_candidate("DLV8FpyxZPQ", 199_000),
        ],
    )
    .await
    .unwrap();
    assert_eq!(hits[0].track().id(), hits[1].track().id());
    assert_eq!(
        repository
            .find_sources(hits[0].track().id())
            .await
            .unwrap()
            .len(),
        2
    );
}

#[sqlx::test]
async fn album_completion_keeps_the_first_position_of_duplicate_responses(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;
    let hits = super::materialize_album_tracks(
        &catalogue,
        &sources,
        vec![
            song_candidate("first", 199_000),
            song_candidate("second", 205_000),
            song_candidate("duplicate", 199_000),
        ],
    )
    .await
    .unwrap();
    assert_eq!(hits[0].track().id(), hits[2].track().id());
    let positions: Vec<i32> =
        sqlx::query_scalar("SELECT position FROM album_tracks ORDER BY position")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(positions, [0, 1]);
}

#[sqlx::test]
async fn provider_tracks_without_a_shared_album_or_with_identifiers_stay_separate(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;
    let first = song_candidate("first", 199_000);
    let other_album = AlbumMetadata::new("Other".to_owned())
        .unwrap()
        .with_album_artist(ArtistCredit::new("Ashe".to_owned(), None).unwrap())
        .with_provider(Some(provider_source("MPREb_other")));
    let recording = CatalogueCandidate::new(
        provider_source("recording"),
        first
            .metadata()
            .clone()
            .with_recording_id(Some("recording-id".to_owned()))
            .unwrap(),
    )
    .with_album(first.album().cloned());
    let isrc = CatalogueCandidate::new(
        provider_source("isrc"),
        first
            .metadata()
            .clone()
            .with_isrc(Some("USABC2600001".to_owned()))
            .unwrap(),
    )
    .with_album(first.album().cloned());
    let hits = materialize(
        &catalogue,
        &sources,
        vec![
            first,
            song_candidate("second", 199_000).with_album(Some(other_album)),
            song_candidate("third", 199_000).with_album(None),
            recording,
            isrc,
        ],
    )
    .await
    .unwrap();
    let ids = hits
        .iter()
        .map(|hit| hit.track().id())
        .collect::<std::collections::HashSet<_>>();
    assert_eq!(ids.len(), 5);
}

#[sqlx::test]
async fn same_album_equal_titles_outside_tolerance_stay_separate(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;
    let hits = materialize(
        &catalogue,
        &sources,
        vec![
            song_candidate("first", 199_000),
            song_candidate("second", 201_001),
        ],
    )
    .await
    .unwrap();
    assert_ne!(hits[0].track().id(), hits[1].track().id());
}

#[sqlx::test]
async fn several_same_album_matches_do_not_attach(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;
    let candidate = song_candidate("third", 199_000);
    let album = materialize_album(&catalogue, &sources, candidate.album().unwrap())
        .await
        .unwrap();
    for video in ["first", "second"] {
        let track = repository
            .get_or_create(provider_source(video), candidate.metadata().clone())
            .await
            .unwrap();
        repository
            .add_album_track(album.id(), track.id(), &AlbumTrack::unplaced())
            .await
            .unwrap();
    }
    let hits = materialize(&catalogue, &sources, vec![candidate])
        .await
        .unwrap();
    assert_eq!(
        repository
            .find_sources(hits[0].track().id())
            .await
            .unwrap()
            .len(),
        1
    );
}

fn album_metadata(external_id: &str, reference: &str) -> AlbumMetadata {
    AlbumMetadata::new("Album".to_owned())
        .unwrap()
        .with_provider(Some(provider_source(external_id)))
        .with_artwork(Some(ArtworkRef::url(reference.to_owned()).unwrap()))
}

#[sqlx::test]
async fn a_new_provider_album_stores_its_artwork(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;

    let album = materialize_album(
        &catalogue,
        &sources,
        &album_metadata("MPREb_album", "https://art.test/first.jpg"),
    )
    .await
    .unwrap();

    let (_, artwork) = repository
        .find_album_artwork(album.id())
        .await
        .unwrap()
        .expect("album artwork is stored");
    assert_eq!(artwork.reference(), "https://art.test/first.jpg");
}

#[sqlx::test]
async fn an_existing_provider_album_replaces_its_artwork(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;

    let first = materialize_album(
        &catalogue,
        &sources,
        &album_metadata("MPREb_album", "https://art.test/first.jpg"),
    )
    .await
    .unwrap();
    let again = materialize_album(
        &catalogue,
        &sources,
        &album_metadata("MPREb_album", "https://art.test/second.jpg"),
    )
    .await
    .unwrap();

    assert_eq!(again.id(), first.id());
    let (_, artwork) = repository
        .find_album_artwork(first.id())
        .await
        .unwrap()
        .expect("album artwork is replaced");
    assert_eq!(artwork.reference(), "https://art.test/second.jpg");
}

struct FailingAlbumArtwork {
    inner: Arc<PostgresTrackRepository>,
}

#[async_trait]
impl SourceLookup for FailingAlbumArtwork {
    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        self.inner.find_sources(track_id).await
    }

    async fn find_album_by_source(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<Album>, TrackRepositoryError> {
        self.inner.find_album_by_source(source).await
    }

    async fn set_album_source_artwork(
        &self,
        _source: &SourceIdentity,
        _artwork: &ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn find_album_artwork(
        &self,
        album_id: &AlbumId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        self.inner.find_album_artwork(album_id).await
    }
}

#[sqlx::test]
async fn a_failed_album_artwork_write_does_not_fail_materialisation(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let sources = Arc::new(FailingAlbumArtwork {
        inner: Arc::clone(&repository),
    }) as Arc<dyn SourceLookup>;

    let album = materialize_album(
        &catalogue,
        &sources,
        &album_metadata("MPREb_album", "https://art.test/first.jpg"),
    )
    .await
    .unwrap();

    assert!(
        repository
            .find_album_artwork(album.id())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        repository
            .find_album_sources(album.id())
            .await
            .unwrap()
            .len(),
        1
    );
}

#[sqlx::test]
async fn an_existing_provider_album_returns_its_refreshed_facts(pool: PgPool) {
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let catalogue = Arc::clone(&repository) as Arc<dyn CatalogueWrite>;
    let sources = Arc::clone(&repository) as Arc<dyn SourceLookup>;

    materialize_album(
        &catalogue,
        &sources,
        &album_metadata("MPREb_album", "https://art.test/first.jpg"),
    )
    .await
    .unwrap();
    let refreshed = album_metadata("MPREb_album", "https://art.test/second.jpg")
        .with_release_type(Some("album".to_owned()))
        .unwrap()
        .with_year(Some(2021))
        .unwrap();

    let album = materialize_album(&catalogue, &sources, &refreshed)
        .await
        .unwrap();

    assert_eq!(album.title(), "Album");
    assert_eq!(album.release_type(), Some("album"));
    assert_eq!(album.year(), Some(2021));
}
