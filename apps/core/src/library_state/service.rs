use std::sync::Arc;

use uuid::Uuid;

use crate::tracks::{
    AlbumId, ArtistId, Starred, StateTarget, TrackId, TrackRepository, TrackRepositoryError,
};

#[derive(Debug, thiserror::Error)]
pub enum LibraryStateError {
    #[error("track repository failed")]
    Repository(#[source] TrackRepositoryError),
}

pub struct LibraryStateService {
    repository: Option<Arc<dyn TrackRepository>>,
}

impl LibraryStateService {
    pub fn new(repository: Arc<dyn TrackRepository>) -> Self {
        Self {
            repository: Some(repository),
        }
    }

    pub fn disabled() -> Self {
        Self { repository: None }
    }

    pub async fn starred(&self, user: Uuid) -> Result<Starred, LibraryStateError> {
        let Some(repository) = &self.repository else {
            return Ok(Starred::new(Vec::new(), Vec::new(), Vec::new()));
        };

        repository
            .starred(user)
            .await
            .map_err(LibraryStateError::Repository)
    }

    pub async fn set_starred(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        starred: bool,
    ) -> Result<(), LibraryStateError> {
        let Some(repository) = &self.repository else {
            return Ok(());
        };

        let exists = match target {
            StateTarget::Track(track_id) => repository
                .find_track(track_id)
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some(),
            StateTarget::Album(album_id) => repository
                .find_album(album_id)
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some(),
            StateTarget::Artist(artist_id) => repository
                .find_artist(artist_id, Some(user))
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some(),
        };
        if !exists {
            return Ok(());
        }

        repository
            .set_starred(user, target, starred)
            .await
            .map_err(LibraryStateError::Repository)
    }

    pub async fn set_rating(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        rating: Option<u8>,
    ) -> Result<(), LibraryStateError> {
        let Some(repository) = &self.repository else {
            return Ok(());
        };

        repository
            .set_rating(user, target, rating)
            .await
            .map_err(LibraryStateError::Repository)
    }

    pub async fn rate_id(
        &self,
        user: Uuid,
        raw_id: &str,
        rating: Option<u8>,
    ) -> Result<bool, LibraryStateError> {
        let Some(repository) = &self.repository else {
            return Ok(false);
        };

        if let Ok(track_id) = raw_id.parse::<TrackId>()
            && repository
                .find_track(&track_id)
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some()
        {
            self.set_rating(user, StateTarget::Track(&track_id), rating)
                .await?;
            return Ok(true);
        }
        if let Ok(album_id) = raw_id.parse::<AlbumId>()
            && repository
                .find_album(&album_id)
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some()
        {
            self.set_rating(user, StateTarget::Album(&album_id), rating)
                .await?;
            return Ok(true);
        }
        if let Ok(artist_id) = raw_id.parse::<ArtistId>()
            && repository
                .find_artist(&artist_id, Some(user))
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some()
        {
            self.set_rating(user, StateTarget::Artist(&artist_id), rating)
                .await?;
            return Ok(true);
        }

        Ok(false)
    }

    pub async fn record_play(
        &self,
        user: Uuid,
        track_id: &TrackId,
        played_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), LibraryStateError> {
        let Some(repository) = &self.repository else {
            return Ok(());
        };
        if repository
            .find_track(track_id)
            .await
            .map_err(LibraryStateError::Repository)?
            .is_none()
        {
            return Ok(());
        }

        repository
            .record_play(user, track_id, played_at)
            .await
            .map_err(LibraryStateError::Repository)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::{
        AlbumMetadata, ArtistCredit, InMemoryTrackRepository, ProviderId, SourceIdentity,
        SourceScope, TrackMetadata,
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

    async fn service() -> (LibraryStateService, Arc<InMemoryTrackRepository>, TrackId) {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let track = repository
            .get_or_create(local_source("path-a"), metadata("Song"))
            .await
            .unwrap();
        let service = LibraryStateService::new(Arc::clone(&repository) as Arc<dyn TrackRepository>);

        (service, repository, track.id().clone())
    }

    #[tokio::test]
    async fn starring_rating_and_playing_are_per_user() {
        let (service, repository, track_id) = service().await;
        let user = Uuid::now_v7();

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

    #[tokio::test]
    async fn unknown_targets_are_skipped() {
        let (service, _repository, _track_id) = service().await;
        let user = Uuid::now_v7();

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

    #[tokio::test]
    async fn ratings_resolve_albums_and_artists() {
        let (service, repository, _track_id) = service().await;
        let user = Uuid::now_v7();
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
}
