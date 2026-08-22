use std::{collections::HashMap, sync::Mutex};

use async_trait::async_trait;

use crate::tracks::{Track, TrackMetadata};

use super::{SourceIdentity, TrackId, TrackRepository, TrackRepositoryError};

struct TrackEntry {
    track: Track,
    sources: Vec<SourceIdentity>,
}

#[derive(Default)]
struct TrackIndexes {
    by_source: HashMap<SourceIdentity, TrackId>,
    by_track_id: HashMap<TrackId, TrackEntry>,
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

            // latest observation wins
            entry.track = Track::new(track_id.clone(), metadata);

            return Ok(entry.track.clone());
        }

        let track_id = TrackId::new();

        let track = Track::new(track_id.clone(), metadata);
        let entry = TrackEntry {
            track: track.clone(),
            sources: vec![source.clone()],
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::{ProviderId, SourceScope, TrackMetadata};

    fn source(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
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
        let first_source = source("source-a");
        let second_source = source("source-b");

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
}
