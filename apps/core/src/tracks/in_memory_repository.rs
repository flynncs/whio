use std::{collections::HashMap, sync::Mutex};

use async_trait::async_trait;

use crate::tracks::{ArtworkRef, ProviderId, Track, TrackMetadata};

use super::{SourceIdentity, TrackId, TrackRepository, TrackRepositoryError};

struct TrackEntry {
    track: Track,
    sources: Vec<SourceIdentity>,
    album_source: Option<String>,
}

#[derive(Default)]
struct TrackIndexes {
    by_source: HashMap<SourceIdentity, TrackId>,
    by_track_id: HashMap<TrackId, TrackEntry>,
    artwork_by_source: HashMap<SourceIdentity, ArtworkRef>,
}

#[derive(Default)]
pub struct InMemoryTrackRepository {
    indexes: Mutex<TrackIndexes>,
}

#[async_trait]
impl TrackRepository for InMemoryTrackRepository {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if let Some(track_id) = indexes.by_source.get(&source).cloned() {
            let entry = indexes
                .by_track_id
                .get_mut(&track_id)
                .ok_or(TrackRepositoryError::Unavailable)?;

            let album = merged_album(source.provider_id(), metadata.album(), entry);
            if album.changed {
                entry.album_source = Some(source.provider_id().as_str().to_owned());
            }

            // latest observation wins for display metadata; the album follows
            // provider precedence.
            let metadata = metadata
                .with_album(album.value)
                .map_err(|_| TrackRepositoryError::InvalidData)?;
            entry.track = Track::new(track_id.clone(), metadata);

            return Ok(entry.track.clone());
        }

        let track_id = TrackId::new();
        let album_source = metadata
            .album()
            .map(|_| source.provider_id().as_str().to_owned());

        let track = Track::new(track_id.clone(), metadata);
        let entry = TrackEntry {
            track: track.clone(),
            sources: vec![source.clone()],
            album_source,
        };

        indexes.by_source.insert(source, track_id.clone());
        indexes.by_track_id.insert(track_id, entry);

        return Ok(track);
    }

    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .by_track_id
            .get(track_id)
            .map(|entry| entry.sources.clone())
            .unwrap_or_default())
    }

    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .by_track_id
            .get(track_id)
            .map(|entry| entry.track.clone()))
    }

    async fn set_source_artwork(
        &self,
        source: &SourceIdentity,
        artwork: &ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        indexes
            .artwork_by_source
            .insert(source.clone(), artwork.clone());
        Ok(())
    }

    async fn find_artwork(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let Some(entry) = indexes.by_track_id.get(track_id) else {
            return Ok(None);
        };

        Ok(entry
            .sources
            .iter()
            .filter_map(|source| {
                indexes
                    .artwork_by_source
                    .get(source)
                    .map(|artwork| (source, artwork))
            })
            .min_by_key(|(source, _)| source.provider_id().precedence())
            .map(|(source, artwork)| (source.clone(), artwork.clone())))
    }
}

struct MergedAlbum {
    value: Option<String>,
    changed: bool,
}

fn merged_album(
    incoming_provider: &ProviderId,
    incoming: Option<&str>,
    entry: &TrackEntry,
) -> MergedAlbum {
    let Some(incoming) = incoming else {
        return MergedAlbum {
            value: entry.track.metadata().album().map(str::to_owned),
            changed: false,
        };
    };

    let existing_rank = entry
        .album_source
        .as_deref()
        .and_then(|source| ProviderId::new(source.to_owned()).ok())
        .map(|provider| provider.precedence());

    let current = entry.track.metadata().album().map(str::to_owned);

    // a higher-precedence provider replaces the album; within one provider the
    // fresher observation wins.
    match current {
        Some(current)
            if existing_rank.is_some_and(|rank| rank < incoming_provider.precedence()) =>
        {
            MergedAlbum {
                value: Some(current),
                changed: false,
            }
        }
        _ => MergedAlbum {
            value: Some(incoming.to_owned()),
            changed: true,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::{SourceScope, TrackMetadata};

    fn source(provider: &str, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            SourceScope::Global,
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn metadata(title: &str) -> TrackMetadata {
        TrackMetadata::new(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
    }

    #[tokio::test]
    async fn repository_maintains_stable_bidirectional_mappings() {
        let repository = InMemoryTrackRepository::default();
        let first_source = source("youtube_music", "source-a");
        let second_source = source("youtube_music", "source-b");

        let first_track = repository
            .get_or_create(first_source.clone(), metadata("First"))
            .await
            .unwrap();
        let repeated_track = repository
            .get_or_create(first_source.clone(), metadata("Updated"))
            .await
            .unwrap();
        let second_track = repository
            .get_or_create(second_source, metadata("Second"))
            .await
            .unwrap();

        assert_eq!(first_track.id(), repeated_track.id());
        assert_ne!(first_track.id(), second_track.id());
        assert_eq!(repeated_track.title(), "Updated");
        assert_eq!(
            repository.find_sources(first_track.id()).await.unwrap(),
            vec![first_source]
        );
    }

    #[tokio::test]
    async fn repository_returns_a_stored_track_by_id() {
        let repository = InMemoryTrackRepository::default();
        let stored = repository
            .get_or_create(source("youtube_music", "source-a"), metadata("First"))
            .await
            .unwrap();

        let found = repository
            .find_track(stored.id())
            .await
            .unwrap()
            .expect("stored track is found");

        assert_eq!(found, stored);
        assert!(
            repository
                .find_track(&TrackId::new())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn repository_returns_no_sources_for_an_unknown_track() {
        let repository = InMemoryTrackRepository::default();

        assert!(
            repository
                .find_sources(&TrackId::new())
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn album_is_stored_and_refreshed_within_a_provider() {
        let repository = InMemoryTrackRepository::default();
        let youtube = source("youtube_music", "vid-1");

        let first = repository
            .get_or_create(
                youtube.clone(),
                metadata("Song")
                    .with_album(Some("First".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(first.album(), Some("First"));

        let refreshed = repository
            .get_or_create(
                youtube,
                metadata("Song")
                    .with_album(Some("Corrected".to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(refreshed.id(), first.id());
        assert_eq!(refreshed.album(), Some("Corrected"));

        let stored = repository
            .find_track(first.id())
            .await
            .unwrap()
            .expect("stored track is found");
        assert_eq!(stored.album(), Some("Corrected"));
    }

    #[tokio::test]
    async fn artwork_resolves_from_the_source_that_stored_it() {
        let repository = InMemoryTrackRepository::default();
        let source = source("youtube_music", "vid-1");

        let track = repository
            .get_or_create(source.clone(), metadata("Song"))
            .await
            .unwrap();

        repository
            .set_source_artwork(
                &source,
                &ArtworkRef::url("https://art.test/ytm.jpg".to_owned()).unwrap(),
            )
            .await
            .unwrap();

        let (owner, artwork) = repository
            .find_artwork(track.id())
            .await
            .unwrap()
            .expect("artwork is stored");
        assert_eq!(owner.provider_id().as_str(), "youtube_music");
        assert_eq!(
            artwork,
            ArtworkRef::url("https://art.test/ytm.jpg".to_owned()).unwrap()
        );
    }
}
