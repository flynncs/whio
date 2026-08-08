use std::{collections::HashMap, sync::Mutex};

use super::{SourceIdentity, TrackId, TrackRepository, TrackRepositoryError};

#[derive(Default)]
struct TrackIndexes {
    by_source: HashMap<SourceIdentity, TrackId>,
    by_track_id: HashMap<TrackId, SourceIdentity>,
}

#[derive(Default)]
pub struct InMemoryTrackRepository {
    indexes: Mutex<TrackIndexes>,
}

impl TrackRepository for InMemoryTrackRepository {
    fn get_or_create_id(&self, source: SourceIdentity) -> Result<TrackId, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if let Some(track_id) = indexes.by_source.get(&source) {
            return Ok(track_id.clone());
        }

        let track_id = TrackId::new();

        indexes.by_source.insert(source.clone(), track_id.clone());
        indexes.by_track_id.insert(track_id.clone(), source);

        Ok(track_id)
    }

    fn find_source(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<SourceIdentity>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.by_track_id.get(track_id).cloned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::ProviderId;

    fn source(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            external_id.to_owned(),
        )
        .unwrap()
    }

    #[test]
    fn repository_maintains_stable_bidirectional_mappings() {
        let repository = InMemoryTrackRepository::default();
        let first_source = source("source-a");
        let second_source = source("source-b");

        let first_id = repository.get_or_create_id(first_source.clone()).unwrap();
        let repeated_id = repository.get_or_create_id(first_source.clone()).unwrap();
        let second_id = repository.get_or_create_id(second_source).unwrap();

        assert_eq!(first_id, repeated_id);
        assert_ne!(first_id, second_id);
        assert_eq!(
            repository.find_source(&first_id).unwrap(),
            Some(first_source)
        );
    }

    #[test]
    fn repository_returns_none_for_an_unknown_track() {
        let repository = InMemoryTrackRepository::default();

        assert_eq!(repository.find_source(&TrackId::new()).unwrap(), None);
    }
}
