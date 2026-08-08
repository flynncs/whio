use async_trait::async_trait;

use crate::{
    playback::PlayableMedia, request::RequestContext, resolvers::ResolverError,
    tracks::SourceIdentity,
};

#[async_trait]
pub trait PlaybackResolver: Send + Sync {
    async fn resolve(
        &self,
        source: &SourceIdentity,
        context: &RequestContext,
    ) -> Result<PlayableMedia, ResolverError>;
}
