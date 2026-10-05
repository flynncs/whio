pub mod artist;
pub mod client;
pub mod config;

#[cfg(test)]
use std::sync::Arc;

use async_trait::async_trait;
use chrono::Utc;
#[cfg(test)]
use reqwest::Url;

#[cfg(test)]
use crate::platform::pacing::CallLimiter;

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

pub struct AudioDbDocuments {
    client: client::AudioDbClient,
}

impl AudioDbDocuments {
    pub fn new(config: &config::Config) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: client::AudioDbClient::new(config.key.clone())?,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_config(
        base_url: Url,
        key: String,
        limiter: Arc<CallLimiter>,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: client::AudioDbClient::with_config(base_url, key, limiter)?,
        })
    }
}

#[async_trait]
impl DocumentProvider for AudioDbDocuments {
    fn provider_id(&self) -> &str {
        "theaudiodb"
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
        let retrieved_at = Utc::now();
        let payload = self
            .client
            .fetch_artist_by_mbid(source.external_id())
            .await?;
        let artist = artist::parse_artist(&payload)?;
        artist::document(source, artist, language, fields, retrieved_at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    const FULL: &str = include_str!("fixtures/audiodb_artist_document.json");
    const LANGUAGES: &str = include_str!("fixtures/audiodb_artist_document_language_member.json");
    const EMPTY: &str = include_str!("fixtures/audiodb_artist_document_empty_biography.json");
    const WIDE: &str = include_str!("fixtures/audiodb_artist_document_wide_only.json");
    const CREATIVE: &str = include_str!("fixtures/audiodb_artist_document_creative_commons.json");

    #[test]
    fn the_audio_db_document_maps_biography_and_both_images() {
        let payload: Value = serde_json::from_str(FULL).unwrap();
        let artist = artist::parse_artist(&payload).unwrap();
        let source = source();
        let doc = artist::document(
            &source,
            artist,
            "en",
            &[
                FieldId::BiographyContent,
                FieldId::ImagePortrait,
                FieldId::ImageWide,
            ],
            Utc::now(),
        )
        .unwrap();
        assert_eq!(
            doc.candidates
                .iter()
                .map(|item| item.field)
                .collect::<Vec<_>>(),
            vec![
                FieldId::BiographyContent,
                FieldId::ImagePortrait,
                FieldId::ImageWide
            ]
        );
        assert!(doc.errors.is_empty());
        assert_eq!(doc.source_revision, None);
        assert_eq!(doc.address, source);
    }

    #[test]
    fn the_requested_language_selects_the_matching_biography() {
        let payload: Value = serde_json::from_str(LANGUAGES).unwrap();
        let artist = artist::parse_artist(&payload).unwrap();
        let (_, text) = artist::select_biography(artist, "de").unwrap();
        assert_eq!(
            text,
            artist.get("strBiographyDE").unwrap().as_str().unwrap()
        );
        let (_, fallback) = artist::select_biography(artist, "xx").unwrap();
        assert_eq!(
            fallback,
            artist.get("strBiographyEN").unwrap().as_str().unwrap()
        );
    }

    #[test]
    fn empty_biography_and_missing_images_are_absence() {
        let payload: Value = serde_json::from_str(EMPTY).unwrap();
        let artist = artist::parse_artist(&payload).unwrap();
        let doc = artist::document(
            &source(),
            artist,
            "en",
            &[FieldId::BiographyContent, FieldId::ImagePortrait],
            Utc::now(),
        )
        .unwrap();
        assert_eq!(doc.candidates.len(), 1);
        assert_eq!(doc.candidates[0].field, FieldId::ImagePortrait);
        assert!(doc.errors.is_empty());
    }

    #[test]
    fn wide_image_falls_back_to_fanart() {
        let payload: Value = serde_json::from_str(WIDE).unwrap();
        let artist = artist::parse_artist(&payload).unwrap();
        let doc =
            artist::document(&source(), artist, "en", &[FieldId::ImageWide], Utc::now()).unwrap();
        assert_eq!(doc.candidates.len(), 1);
        assert_eq!(doc.candidates[0].image.as_ref().unwrap().role, "wide");
    }

    #[test]
    fn creative_commons_changes_the_image_assertion_only() {
        let payload: Value = serde_json::from_str(CREATIVE).unwrap();
        let artist = artist::parse_artist(&payload).unwrap();
        let doc = artist::document(
            &source(),
            artist,
            "en",
            &[FieldId::BiographyContent, FieldId::ImagePortrait],
            Utc::now(),
        )
        .unwrap();
        assert_eq!(doc.candidates[0].licence.id, "theaudiodb");
        assert_eq!(doc.candidates[1].licence.id, "commons:CC BY 4.0");
    }

    #[test]
    fn biography_content_is_omitted_over_its_byte_cap() {
        let mut artist = serde_json::from_str::<Value>(FULL)
            .unwrap()
            .get("artists")
            .unwrap()[0]
            .as_object()
            .unwrap()
            .clone();
        artist.insert(
            "strBiographyEN".to_owned(),
            Value::String("x".repeat(65_537)),
        );
        let document = artist::document(
            &source(),
            &artist,
            "en",
            &[FieldId::BiographyContent],
            Utc::now(),
        )
        .unwrap();
        assert!(document.candidates.is_empty());
        assert_eq!(document.errors.len(), 1);
        assert_eq!(document.errors[0].field, FieldId::BiographyContent);
        assert_eq!(document.errors[0].code, "resolution_failed");
        assert!(!document.errors[0].retryable);
    }

    #[test]
    fn the_test_constructor_accepts_a_controlled_host() {
        let base = Url::parse("http://127.0.0.1:4321/").unwrap();
        let docs = AudioDbDocuments::with_config(
            base,
            "key".to_owned(),
            Arc::new(CallLimiter::new(std::time::Duration::ZERO)),
        )
        .unwrap();
        assert_eq!(docs.provider_id(), "theaudiodb");
    }

    fn source() -> SourceIdentity {
        SourceIdentity::new(
            crate::features::music_library::domain::ProviderId::new("theaudiodb".to_owned())
                .unwrap(),
            crate::features::music_library::domain::SourceScope::Global,
            "cc197bad-dc9c-440d-a5b5-d52ba2e14234".to_owned(),
        )
        .unwrap()
    }
}

#[cfg(test)]
#[path = "tests.rs"]
mod ported_tests;
