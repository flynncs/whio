use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;

use crate::features::{
    metadata::{
        capabilities::{CapabilityError, CapabilitySource, Snapshot},
        domain::FieldId,
        refresh::{DocumentError, DocumentOutcome, MetadataDocuments},
        service::MetadataProfile,
    },
    music_library::domain::SourceIdentity,
};

#[async_trait]
pub trait DocumentProvider: Send + Sync {
    fn provider_id(&self) -> &str;
    async fn advertised(&self) -> Result<bool, CapabilityError>;
    async fn fetch(
        &self,
        source: &SourceIdentity,
        profile: &MetadataProfile,
        language: &str,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError>;
}

#[derive(Debug, thiserror::Error)]
#[error("document provider `{0}` is registered twice")]
pub struct DuplicateProvider(pub String);

pub struct DocumentProviders {
    providers: BTreeMap<String, Arc<dyn DocumentProvider>>,
}

impl DocumentProviders {
    pub fn new(providers: Vec<Arc<dyn DocumentProvider>>) -> Result<Self, DuplicateProvider> {
        let mut registered = BTreeMap::new();
        for provider in providers {
            let id = provider.provider_id().to_owned();
            if registered.insert(id.clone(), provider).is_some() {
                return Err(DuplicateProvider(id));
            }
        }
        Ok(Self {
            providers: registered,
        })
    }
}

#[async_trait]
impl MetadataDocuments for DocumentProviders {
    async fn fetch(
        &self,
        source: &SourceIdentity,
        profile: &MetadataProfile,
        language: &str,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        let id = source.provider_id().as_str();
        self.providers
            .get(id)
            .ok_or(DocumentError::Unsupported)?
            .fetch(source, profile, language, fields)
            .await
    }
}

#[async_trait]
impl CapabilitySource for DocumentProviders {
    async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
        if self.providers.is_empty() {
            return Err(CapabilityError::Disabled);
        }
        let mut advertised = Vec::new();
        let mut reached = false;
        for (id, provider) in &self.providers {
            match provider.advertised().await {
                Ok(true) => {
                    reached = true;
                    advertised.push(id.clone());
                }
                Ok(false) => reached = true,
                Err(_) => {}
            }
        }
        if reached {
            Ok(Snapshot::new(advertised))
        } else {
            Err(CapabilityError::Unavailable)
        }
    }
}
