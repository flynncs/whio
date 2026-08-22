use std::sync::Arc;

use crate::{
    playback::{PlayableMedia, PlaybackResolver},
    request::RequestContext,
    resolvers::ResolverError,
    tracks::{TrackId, TrackRepository, TrackRepositoryError},
};

#[derive(Debug, thiserror::Error)]
pub enum PlaybackError {
    #[error("track was not found")]
    TrackNotFound,

    #[error("track repository failed")]
    Repository(#[source] TrackRepositoryError),

    #[error("playback resolver failed")]
    Resolver(#[source] ResolverError),
}

pub struct PlaybackService {
    resolver: Arc<dyn PlaybackResolver>,
    track_repository: Arc<dyn TrackRepository>,
}

impl PlaybackService {
    pub fn new(
        resolver: Arc<dyn PlaybackResolver>,
        track_repository: Arc<dyn TrackRepository>,
    ) -> Self {
        Self {
            resolver,
            track_repository,
        }
    }

    pub async fn resolve(
        &self,
        track_id: &TrackId,
        context: &RequestContext,
    ) -> Result<PlayableMedia, PlaybackError> {
        let source = self
            .track_repository
            .find_sources(track_id)
            .await
            .map_err(PlaybackError::Repository)?
            .into_iter()
            .next()
            .ok_or(PlaybackError::TrackNotFound)?;

        self.resolver
            .resolve(&source, context)
            .await
            .map_err(PlaybackError::Resolver)
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::HashMap, sync::Mutex};

    use async_trait::async_trait;

    use super::*;
    use crate::{
        playback::{MediaMetadata, PlaybackUrl},
        request::RequestId,
        tracks::{
            InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, Track, TrackMetadata,
        },
    };

    struct StubResolver {
        response: Mutex<Option<Result<PlayableMedia, ResolverError>>>,
        source: Mutex<Option<SourceIdentity>>,
    }

    impl StubResolver {
        fn new(response: Result<PlayableMedia, ResolverError>) -> Self {
            Self {
                response: Mutex::new(Some(response)),
                source: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl PlaybackResolver for StubResolver {
        async fn resolve(
            &self,
            source: &SourceIdentity,
            _context: &RequestContext,
        ) -> Result<PlayableMedia, ResolverError> {
            *self.source.lock().unwrap() = Some(source.clone());
            self.response.lock().unwrap().take().unwrap()
        }
    }

    struct FailingTrackRepository;

    #[async_trait]
    impl TrackRepository for FailingTrackRepository {
        async fn get_or_create(
            &self,
            _source: SourceIdentity,
            _metadata: TrackMetadata,
        ) -> Result<Track, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn find_sources(
            &self,
            _track_id: &TrackId,
        ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }
    }

    fn source() -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            "source-123".to_owned(),
        )
        .unwrap()
    }

    fn track_metadata(title: &str) -> TrackMetadata {
        TrackMetadata::new(title.to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
    }

    fn playable_media() -> PlayableMedia {
        PlayableMedia::new(
            PlaybackUrl::new("https://media.example.test/audio".to_owned()).unwrap(),
            HashMap::new(),
            None,
            MediaMetadata::new(None, None, Some("opus".to_owned()), Some(128.0), None).unwrap(),
        )
    }

    fn context() -> RequestContext {
        RequestContext::new(RequestId::new("request-123".to_owned()).unwrap())
    }

    #[tokio::test]
    async fn resolve_finds_source_and_returns_playable_media() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let source = source();
        let track_id = repository
            .get_or_create(source.clone(), track_metadata("Title"))
            .await
            .unwrap()
            .id()
            .clone();
        let expected = playable_media();
        let resolver = Arc::new(StubResolver::new(Ok(expected.clone())));
        let service = PlaybackService::new(resolver.clone(), repository);

        let result = service.resolve(&track_id, &context()).await.unwrap();

        assert_eq!(result, expected);
        assert_eq!(resolver.source.lock().unwrap().as_ref(), Some(&source));
    }

    #[tokio::test]
    async fn resolve_rejects_an_unknown_whio_track() {
        let resolver = Arc::new(StubResolver::new(Ok(playable_media())));
        let service = PlaybackService::new(
            resolver.clone(),
            Arc::new(InMemoryTrackRepository::default()),
        );

        let result = service.resolve(&TrackId::new(), &context()).await;

        assert!(matches!(result, Err(PlaybackError::TrackNotFound)));
        assert!(resolver.source.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn resolve_preserves_repository_errors() {
        let resolver = Arc::new(StubResolver::new(Ok(playable_media())));
        let service = PlaybackService::new(resolver, Arc::new(FailingTrackRepository));

        let result = service.resolve(&TrackId::new(), &context()).await;

        assert!(matches!(
            result,
            Err(PlaybackError::Repository(TrackRepositoryError::Unavailable))
        ));
    }

    #[tokio::test]
    async fn resolve_preserves_resolver_errors() {
        let repository = Arc::new(InMemoryTrackRepository::default());
        let track_id = repository
            .get_or_create(source(), track_metadata("Title"))
            .await
            .unwrap()
            .id()
            .clone();
        let resolver = StubResolver::new(Err(ResolverError::ProviderUnavailable));
        let service = PlaybackService::new(Arc::new(resolver), repository);

        let result = service.resolve(&track_id, &context()).await;

        assert!(matches!(
            result,
            Err(PlaybackError::Resolver(ResolverError::ProviderUnavailable))
        ));
    }
}
