pub mod artist;
pub mod client;
pub mod config;
pub mod wikipedia;

use crate::features::{
    metadata::{
        capabilities::CapabilityError,
        domain::FieldId,
        providers::DocumentProvider,
        refresh::{DocumentError, DocumentOutcome},
        service::MetadataProfile,
    },
    music_library::domain::SourceIdentity,
};
use async_trait::async_trait;

#[async_trait]
impl DocumentProvider for artist::MusicBrainzDocuments {
    fn provider_id(&self) -> &str {
        self.provider_id()
    }
    async fn advertised(&self) -> Result<bool, CapabilityError> {
        Ok(true)
    }
    async fn fetch(
        &self,
        source: &SourceIdentity,
        _profile: &MetadataProfile,
        language: &str,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        artist::MusicBrainzDocuments::fetch(self, source, language, fields).await
    }
}
