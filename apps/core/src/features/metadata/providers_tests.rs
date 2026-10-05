use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;

use super::{
    CapabilityError, CapabilitySource, DocumentError, DocumentProvider, DocumentProviders,
    MetadataProfile, Snapshot,
};
use crate::features::{
    metadata::{DocumentOutcome, MetadataDocuments, domain::FieldId},
    music_library::domain::SourceIdentity,
};

struct Stub {
    id: &'static str,
    result: Result<bool, CapabilityError>,
    calls: Arc<Mutex<Vec<&'static str>>>,
}

#[async_trait]
impl DocumentProvider for Stub {
    fn provider_id(&self) -> &str {
        self.id
    }
    async fn advertised(&self) -> Result<bool, CapabilityError> {
        self.calls.lock().unwrap().push(self.id);
        self.result.clone()
    }
    async fn fetch(
        &self,
        _: &SourceIdentity,
        _: &MetadataProfile,
        _: &str,
        _: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        Err(DocumentError::NotFound)
    }
}

fn stub(
    id: &'static str,
    result: Result<bool, CapabilityError>,
    calls: Arc<Mutex<Vec<&'static str>>>,
) -> Arc<dyn DocumentProvider> {
    Arc::new(Stub { id, result, calls })
}

#[tokio::test]
async fn duplicate_ids_are_rejected() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let result = DocumentProviders::new(vec![
        stub("a", Ok(true), Arc::clone(&calls)),
        stub("a", Ok(false), calls),
    ]);
    assert!(matches!(result, Err(error) if error.0 == "a"));
}

#[tokio::test]
async fn unknown_document_provider_is_unsupported_without_a_call() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers =
        DocumentProviders::new(vec![stub("known", Ok(true), Arc::clone(&calls))]).unwrap();
    let source = crate::features::music_library::domain::SourceIdentity::new(
        crate::features::music_library::domain::ProviderId::new("unknown".to_owned()).unwrap(),
        crate::features::music_library::domain::SourceScope::Global,
        "id".to_owned(),
    )
    .unwrap();
    assert_eq!(
        MetadataDocuments::fetch(
            &providers,
            &source,
            &MetadataProfile::artist_display(),
            "en",
            &[]
        )
        .await,
        Err(DocumentError::Unsupported)
    );
    assert!(calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn capabilities_are_sequential_and_include_only_true_providers() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = DocumentProviders::new(vec![
        stub("z", Ok(true), Arc::clone(&calls)),
        stub("a", Ok(false), Arc::clone(&calls)),
    ])
    .unwrap();
    assert_eq!(
        CapabilitySource::fetch(&providers).await.unwrap(),
        Snapshot::new(["z".to_owned()])
    );
    assert_eq!(*calls.lock().unwrap(), vec!["a", "z"]);
}

#[tokio::test]
async fn no_providers_are_disabled() {
    let empty = DocumentProviders::new(Vec::new()).unwrap();
    assert_eq!(
        CapabilitySource::fetch(&empty).await,
        Err(CapabilityError::Disabled)
    );
}

#[tokio::test]
async fn a_failing_provider_is_absent_while_others_advertise() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = DocumentProviders::new(vec![
        stub("a", Err(CapabilityError::InvalidData), Arc::clone(&calls)),
        stub("b", Ok(true), Arc::clone(&calls)),
    ])
    .unwrap();
    assert_eq!(
        CapabilitySource::fetch(&providers).await.unwrap(),
        Snapshot::new(["b".to_owned()])
    );
    assert_eq!(*calls.lock().unwrap(), vec!["a", "b"]);
}

#[tokio::test]
async fn every_failing_provider_is_unavailable() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let providers = DocumentProviders::new(vec![
        stub("a", Err(CapabilityError::InvalidData), Arc::clone(&calls)),
        stub("b", Err(CapabilityError::Unavailable), Arc::clone(&calls)),
    ])
    .unwrap();
    assert_eq!(
        CapabilitySource::fetch(&providers).await,
        Err(CapabilityError::Unavailable)
    );
}

#[tokio::test]
async fn all_false_providers_produce_an_empty_snapshot() {
    let providers =
        DocumentProviders::new(vec![stub("a", Ok(false), Arc::new(Mutex::new(Vec::new())))])
            .unwrap();
    assert_eq!(
        CapabilitySource::fetch(&providers).await.unwrap(),
        Snapshot::default()
    );
}

#[test]
fn capability_ttl_remains_five_minutes() {
    assert_eq!(
        super::capabilities::CAPABILITY_TTL,
        Duration::from_secs(300)
    );
}
