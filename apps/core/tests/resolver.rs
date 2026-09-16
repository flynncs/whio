// the HTTP resolver client is gone; this suite pins the DTO mapping and
// disabled-adapter behavior it used to cover instead
use std::collections::HashMap;

use whio_core::{
    catalogue::{CatalogueCandidate, CatalogueProvider, CatalogueSearch, SearchLimit, SearchQuery},
    playback::{
        PlayableMedia, PlaybackProvider, PlaybackSource, ValidationError as PlaybackValidationError,
    },
    providers::ProviderError,
    providers::YtmAdapter,
    request::{RequestContext, RequestId},
    tracks::{ProviderId, SourceIdentity, SourceScope, ValidationError as TrackValidationError},
};
use whio_resolver_api::models::{
    CatalogueCandidate as CatalogueCandidateDto, MediaMetadata as MediaMetadataDto,
    PlaybackResolveResponse as PlaybackResolveResponseDto, SourceIdentity as SourceIdentityDto,
};

fn candidate_dto() -> CatalogueCandidateDto {
    CatalogueCandidateDto {
        source: Box::new(SourceIdentityDto::new(
            "example_music".to_owned(),
            "example-source-id".to_owned(),
        )),
        title: "Instant Crush".to_owned(),
        artists: vec!["Daft Punk".to_owned()],
        duration_ms: Some(Some(337_000)),
    }
}

fn playback_dto() -> PlaybackResolveResponseDto {
    PlaybackResolveResponseDto {
        url: "https://media.example.test/audio".to_owned(),
        headers: HashMap::from([("User-Agent".to_owned(), "whio-test".to_owned())]),
        expires_at: Some(Some("2026-08-08T12:00:00+12:00".parse().unwrap())),
        media: Box::new(MediaMetadataDto {
            content_type: Some(Some("audio/webm".to_owned())),
            content_length_bytes: Some(Some(1_024)),
            codec: Some(Some("opus".to_owned())),
            bitrate_kbps: Some(Some(128.5)),
            duration_ms: Some(Some(337_250)),
        }),
    }
}

fn search() -> CatalogueSearch {
    CatalogueSearch::new(
        SearchQuery::new("Instant Crush".to_owned()).unwrap(),
        SearchLimit::new(5).unwrap(),
    )
}

fn context() -> RequestContext {
    RequestContext::new(RequestId::new("request-123".to_owned()).unwrap())
}

fn playback_source() -> PlaybackSource {
    PlaybackSource::from_identity(
        SourceIdentity::new(
            ProviderId::new("youtube_music".to_owned()).unwrap(),
            SourceScope::Global,
            "source-123".to_owned(),
        )
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn valid_candidate_dto_maps_to_domain_candidate() {
    let candidate = CatalogueCandidate::try_from(candidate_dto()).unwrap();

    assert_eq!(candidate.source().provider_id().as_str(), "example_music");
    assert_eq!(candidate.source().external_id(), "example-source-id");
    assert_eq!(candidate.title(), "Instant Crush");
    assert_eq!(candidate.artists(), ["Daft Punk"]);
    assert_eq!(candidate.duration_ms(), Some(337_000));
}

#[test]
fn invalid_candidate_dto_is_rejected() {
    let mut dto = candidate_dto();
    dto.duration_ms = Some(Some(-1));

    assert!(matches!(
        CatalogueCandidate::try_from(dto),
        Err(TrackValidationError::InvalidDuration)
    ));
}

#[test]
fn valid_playback_dto_maps_to_domain_media() {
    let playback = PlayableMedia::try_from(playback_dto()).unwrap();
    let (url, headers, expires_at, metadata) = playback.into_parts();

    assert_eq!(url.as_url().as_str(), "https://media.example.test/audio");
    assert_eq!(headers["User-Agent"], "whio-test");
    assert_eq!(
        expires_at.unwrap().to_rfc3339(),
        "2026-08-08T00:00:00+00:00"
    );
    assert_eq!(metadata.duration_ms(), Some(337_250));
}

#[test]
fn invalid_playback_dto_is_rejected() {
    let mut dto = playback_dto();
    dto.media.duration_ms = Some(Some(-1));

    assert!(matches!(
        PlayableMedia::try_from(dto),
        Err(PlaybackValidationError::InvalidDuration)
    ));
}

#[tokio::test]
async fn disabled_adapter_search_reports_disabled() {
    let adapter = YtmAdapter::disabled();

    assert!(matches!(
        adapter.search(&search(), None, &context()).await,
        Err(ProviderError::Disabled)
    ));
}

#[tokio::test]
async fn disabled_adapter_resolve_reports_disabled() {
    let adapter = YtmAdapter::disabled();

    assert!(matches!(
        adapter.resolve(&playback_source(), None, &context()).await,
        Err(ProviderError::Disabled)
    ));
}
