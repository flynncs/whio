use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    playback::{LocalMedia, PlaybackProvider, PlaybackSource, ResolvedPlayback},
    providers::ProviderError,
    request::RequestContext,
    source_path::local_path,
    tracks::{ProviderKind, SourceLookup, SourceScope, TrackRepositoryError},
};

pub struct LocalPlaybackProvider {
    sources: Arc<dyn SourceLookup>,
}

impl LocalPlaybackProvider {
    pub fn new(sources: Arc<dyn SourceLookup>) -> Self {
        Self { sources }
    }
}

#[async_trait]
impl PlaybackProvider for LocalPlaybackProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Local
    }

    async fn resolve(
        &self,
        source: &PlaybackSource,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, ProviderError> {
        let PlaybackSource::Local(source) = source else {
            return Ok(None);
        };
        let SourceScope::Named(root) = source.identity().source_scope() else {
            return Ok(None);
        };

        let Some(facts) = self
            .sources
            .find_source_file_facts(source.identity())
            .await
            .map_err(repository_error)?
        else {
            return Ok(None);
        };
        if facts.missing_at.is_some() {
            return Ok(None);
        }

        let Some(relative) = facts.path.as_deref() else {
            return Ok(None);
        };
        let Some(absolute) = local_path(root, relative) else {
            return Ok(None);
        };

        let length = match tokio::fs::metadata(&absolute).await {
            Ok(metadata) if metadata.is_file() => metadata.len(),
            Ok(_) => return Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(ProviderError::request(error)),
        };
        if length == 0 {
            return Ok(None);
        }

        let content_type = facts
            .content_type
            .clone()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                facts
                    .suffix
                    .as_deref()
                    .and_then(content_type_for_suffix)
                    .map(str::to_owned)
            });

        let media = LocalMedia::new(absolute, length, content_type)
            .map_err(ProviderError::InvalidPlaybackData)?;
        Ok(Some(ResolvedPlayback::Local(media)))
    }
}

fn content_type_for_suffix(suffix: &str) -> Option<&'static str> {
    match suffix {
        "flac" => Some("audio/flac"),
        "mp3" => Some("audio/mpeg"),
        "m4a" | "mp4" => Some("audio/mp4"),
        "ogg" | "oga" | "opus" => Some("audio/ogg"),
        "wav" => Some("audio/wav"),
        "aiff" | "aif" => Some("audio/aiff"),
        "ape" => Some("audio/x-ape"),
        "wv" => Some("audio/x-wavpack"),
        "mpc" => Some("audio/x-musepack"),
        _ => None,
    }
}

fn repository_error(error: TrackRepositoryError) -> ProviderError {
    match error {
        TrackRepositoryError::Unavailable => ProviderError::ProviderUnavailable,
        TrackRepositoryError::InvalidData => ProviderError::Internal,
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::{
        request::RequestId,
        tracks::{
            CatalogueWrite, InMemoryTrackRepository, ProviderId, SourceFileFacts, SourceIdentity,
        },
    };

    fn context() -> RequestContext {
        RequestContext::new(RequestId::new("request-123".to_owned()).unwrap())
    }

    fn root(label: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("whio-local-provider-{label}-{}", Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn local_source(root: &Path, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named(root.display().to_string()),
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn facts(path: &str) -> SourceFileFacts {
        SourceFileFacts {
            path: Some(path.to_owned()),
            size_bytes: 10,
            modified_at: None,
            missing_at: None,
            content_type: None,
            suffix: Some("flac".to_owned()),
            acoustid_fingerprint: None,
        }
    }

    async fn provider_with(
        root: &Path,
        facts: SourceFileFacts,
    ) -> (LocalPlaybackProvider, SourceIdentity) {
        let store = Arc::new(InMemoryTrackRepository::default());
        let source = local_source(root, "recording-1");
        store.set_source_file_facts(&source, &facts).await.unwrap();
        (
            LocalPlaybackProvider::new(Arc::clone(&store) as Arc<dyn SourceLookup>),
            source,
        )
    }

    #[tokio::test]
    async fn resolves_a_scanned_file_to_local_media() {
        let root = root("resolve");
        let file = root.join("a.flac");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac"),
            &file,
        )
        .unwrap();
        let size = std::fs::metadata(&file).unwrap().len();
        let (provider, source) = provider_with(&root, facts("a.flac")).await;
        let source = PlaybackSource::from_identity(source).unwrap();

        let resolved = provider
            .resolve(&source, None, &context())
            .await
            .unwrap()
            .expect("the file exists");

        match resolved {
            ResolvedPlayback::Local(media) => {
                assert_eq!(media.path(), file);
                assert_eq!(media.content_length(), size);
                assert_eq!(media.content_type(), Some("audio/flac"));
            }
            other => panic!("expected local media, got {other:?}"),
        }

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_stored_content_type_wins_over_the_suffix() {
        let root = root("content-type");
        std::fs::write(root.join("a.flac"), b"0123456789").unwrap();
        let mut facts = facts("a.flac");
        facts.content_type = Some("audio/x-custom".to_owned());
        let (provider, source) = provider_with(&root, facts).await;
        let source = PlaybackSource::from_identity(source).unwrap();

        let resolved = provider
            .resolve(&source, None, &context())
            .await
            .unwrap()
            .unwrap();

        let ResolvedPlayback::Local(media) = resolved else {
            panic!("expected local media");
        };
        assert_eq!(media.content_type(), Some("audio/x-custom"));

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_source_without_file_facts_resolves_to_nothing() {
        let root = root("no-facts");
        let store = Arc::new(InMemoryTrackRepository::default());
        let provider = LocalPlaybackProvider::new(Arc::clone(&store) as Arc<dyn SourceLookup>);
        let source = PlaybackSource::from_identity(local_source(&root, "recording-1")).unwrap();

        assert!(
            provider
                .resolve(&source, None, &context())
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn a_vanished_file_or_a_missing_marker_resolves_to_nothing() {
        let root = root("missing");
        std::fs::write(root.join("a.flac"), b"0123456789").unwrap();

        let (provider, source) = provider_with(&root, facts("gone.flac")).await;
        let source = PlaybackSource::from_identity(source).unwrap();
        assert!(
            provider
                .resolve(&source, None, &context())
                .await
                .unwrap()
                .is_none()
        );

        let mut marked = facts("a.flac");
        marked.missing_at = Some(chrono::Utc::now());
        let (provider, source) = provider_with(&root, marked).await;
        let source = PlaybackSource::from_identity(source).unwrap();
        assert!(
            provider
                .resolve(&source, None, &context())
                .await
                .unwrap()
                .is_none()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_path_that_leaves_the_media_root_resolves_to_nothing() {
        let root = root("escape");
        let (provider, source) = provider_with(&root, facts("../outside.flac")).await;
        let source = PlaybackSource::from_identity(source).unwrap();

        assert!(
            provider
                .resolve(&source, None, &context())
                .await
                .unwrap()
                .is_none()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[tokio::test]
    async fn a_remote_source_is_not_the_local_provider_s_job() {
        let root = root("remote");
        let (provider, _) = provider_with(&root, facts("a.flac")).await;
        let youtube = SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            "video-1".to_owned(),
        )
        .unwrap();
        let source = PlaybackSource::from_identity(youtube).unwrap();

        assert!(
            provider
                .resolve(&source, None, &context())
                .await
                .unwrap()
                .is_none()
        );

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn suffixes_map_to_the_content_types_clients_expect() {
        assert_eq!(content_type_for_suffix("flac"), Some("audio/flac"));
        assert_eq!(content_type_for_suffix("mp3"), Some("audio/mpeg"));
        assert_eq!(content_type_for_suffix("opus"), Some("audio/ogg"));
        assert_eq!(content_type_for_suffix("wv"), Some("audio/x-wavpack"));
        assert_eq!(content_type_for_suffix("lrc"), None);
    }
}
