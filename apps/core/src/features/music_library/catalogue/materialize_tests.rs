use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;

use super::materialize_album;
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
