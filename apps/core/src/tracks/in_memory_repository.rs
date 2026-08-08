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
