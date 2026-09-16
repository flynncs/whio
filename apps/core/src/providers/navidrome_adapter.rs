use std::sync::Arc;

use async_trait::async_trait;
use uuid::Uuid;

use super::{UpstreamError, UpstreamTarget, UpstreamTransport};
use crate::{
    catalogue::{CatalogueCandidate, CatalogueProvider, CatalogueSearch},
    identity::{
        secrets::CredentialKey,
        upstream::{UpstreamLinkStore, UpstreamStoreError, decrypt_password},
    },
    media::is_registered_media_url,
    playback::PlaybackProvider,
    playback::{
        DeliveryStrategy, MediaMetadata, NavidromeSource, PlayableMedia, PlaybackSource,
        PlaybackUrl, ResolvedPlayback, ValidationError as PlaybackValidationError,
    },
    providers::ProviderError,
    request::RequestContext,
    tracks::{ProviderId, ProviderKind, SourceIdentity, SourceScope, TrackMetadata},
};

pub struct NavidromeAdapter {
    links: Arc<dyn UpstreamLinkStore>,
    key: CredentialKey,
    transport: Arc<dyn UpstreamTransport>,
}

impl NavidromeAdapter {
    pub fn new(
        links: Arc<dyn UpstreamLinkStore>,
        key: CredentialKey,
        transport: Arc<dyn UpstreamTransport>,
    ) -> Self {
        Self {
            links,
            key,
            transport,
        }
    }
}

#[async_trait]
impl CatalogueProvider for NavidromeAdapter {
    async fn search(
        &self,
        search: &CatalogueSearch,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        let Some(user) = user else {
            return Ok(Vec::new());
        };
        let links = self.links.links_for_user(user).await.map_err(access)?;
        let provider =
            ProviderId::new("navidrome".to_owned()).map_err(ProviderError::InvalidCatalogueData)?;
        let mut candidates = Vec::new();
        for link in links {
            let password = decrypt_password(&self.key, link.link_id, &link.encrypted_password)
                .map_err(access)?;
            let target = UpstreamTarget {
                base_url: link.base_url.clone(),
                username: link.username.clone(),
                password,
            };
            let page = match self.transport.search(&target, search, context).await {
                Ok(page) => page,
                Err(UpstreamError::Auth) => {
                    self.links
                        .mark_stale(user, &link.server_id)
                        .await
                        .map_err(access)?;
                    return Err(ProviderError::CredentialsRejected);
                }
                Err(UpstreamError::Forbidden | UpstreamError::InvalidRequest) => {
                    return Err(ProviderError::InvalidRequest);
                }
                Err(UpstreamError::NotFound) => return Err(ProviderError::SourceNotFound),
                Err(UpstreamError::InvalidResponse) => {
                    return Err(ProviderError::malformed(UpstreamError::InvalidResponse));
                }
                Err(UpstreamError::Unavailable) => {
                    return Err(ProviderError::ProviderUnavailable);
                }
            };
            for candidate in page {
                let Ok(source) = SourceIdentity::new(
                    provider.clone(),
                    SourceScope::Named(link.server_scope.clone()),
                    candidate.external_id,
                ) else {
                    continue;
                };
                let Ok(metadata) =
                    TrackMetadata::new(candidate.title, candidate.artists, candidate.duration_ms)
                else {
                    continue;
                };
                candidates.push(CatalogueCandidate::new(source, metadata));
            }
        }
        Ok(candidates)
    }
}

#[async_trait]
impl PlaybackProvider for NavidromeAdapter {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Navidrome
    }

    async fn resolve(
        &self,
        source: &PlaybackSource,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, ProviderError> {
        let PlaybackSource::Navidrome(source) = source else {
            return Ok(None);
        };
        let Some(user) = user else {
            return Ok(None);
        };
        let Some(link) = self
            .links
            .link_for_server(user, source.server_id())
            .await
            .map_err(access)?
        else {
            return Ok(None);
        };
        let password =
            decrypt_password(&self.key, link.link_id, &link.encrypted_password).map_err(access)?;
        let target = UpstreamTarget {
            base_url: link.base_url.clone(),
            username: link.username.clone(),
            password,
        };
        match self
            .transport
            .resolve(&target, source.identity().external_id(), context)
            .await
        {
            Ok(url) => {
                let media = media_from_url(&target.base_url, url)
                    .map_err(ProviderError::InvalidPlaybackData)?;
                Ok(Some(ResolvedPlayback {
                    media,
                    strategy: DeliveryStrategy::Direct,
                }))
            }
            Err(UpstreamError::Auth) => Err(self.rejected(source, user).await),
            Err(UpstreamError::NotFound) => Err(ProviderError::SourceNotFound),
            Err(_) => Err(ProviderError::ProviderUnavailable),
        }
    }

    async fn credential_rejected(
        &self,
        source: &PlaybackSource,
        user: Option<&Uuid>,
    ) -> ProviderError {
        let PlaybackSource::Navidrome(source) = source else {
            return ProviderError::ProviderUnavailable;
        };
        let Some(user) = user else {
            return ProviderError::ProviderUnavailable;
        };
        self.rejected(source, user).await
    }
}

impl NavidromeAdapter {
    async fn rejected(&self, source: &NavidromeSource, user: &Uuid) -> ProviderError {
        if let Err(error) = self.links.mark_stale(user, source.server_id()).await {
            return access(error);
        }
        ProviderError::CredentialsRejected
    }
}

/// linked-account failures keep the underlying cause in the log and expose only
/// a safe category to the rest of the application.
fn access(error: UpstreamStoreError) -> ProviderError {
    tracing::error!(%error, "linked account access failed");
    ProviderError::Access
}

fn media_from_url(base_url: &str, url: String) -> Result<PlayableMedia, PlaybackValidationError> {
    let media_url = PlaybackUrl::new(url)?;
    let base = reqwest::Url::parse(base_url).map_err(|_| PlaybackValidationError::InvalidUrl)?;
    if !is_registered_media_url(&base, media_url.as_url()) {
        return Err(PlaybackValidationError::InvalidUrl);
    }
    let metadata =
        MediaMetadata::new(None, None, None, None, None).expect("empty media metadata is valid");
    Ok(PlayableMedia::new(
        media_url,
        Default::default(),
        None,
        metadata,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_resolved_urls_outside_registered_origin() {
        let target = "http://navidrome.example/music";
        let error = media_from_url(target, "http://other.example/audio".to_owned()).unwrap_err();
        assert!(matches!(error, PlaybackValidationError::InvalidUrl));
    }

    #[test]
    fn accepts_private_registered_origin() {
        let target = "http://127.0.0.1:4533/music";
        let media = media_from_url(target, "http://127.0.0.1:4533/audio".to_owned()).unwrap();
        assert_eq!(media.url().as_url().as_str(), "http://127.0.0.1:4533/audio");
    }
}
