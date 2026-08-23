use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
mod support;

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use support::{credential_service, router};
use tower::ServiceExt;
use whio_core::{
    catalogue::{CatalogueCandidate, CatalogueResolver, CatalogueSearch, CatalogueService},
    media::{MediaBody, MediaFetchError, MediaFetcher, MediaInfo},
    playback::{MediaMetadata, PlayableMedia, PlaybackResolver, PlaybackService, PlaybackUrl},
    playback_stream::PlaybackStreamService,
    request::RequestContext,
    resolver::ResolverError,
    tracks::{
        InMemoryTrackRepository, ProviderId, SourceIdentity, SourceScope, TrackId, TrackMetadata,
    },
};

struct StubResolver {
    candidate: CatalogueCandidate,
    playback: Mutex<Option<PlayableMedia>>,
    resolved_source: Mutex<Option<SourceIdentity>>,
}

#[async_trait]
impl CatalogueResolver for StubResolver {
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ResolverError> {
        Ok(vec![self.candidate.clone()])
    }
}

#[async_trait]
impl PlaybackResolver for StubResolver {
    async fn resolve(
        &self,
        source: &SourceIdentity,
        _context: &RequestContext,
    ) -> Result<PlayableMedia, ResolverError> {
        *self.resolved_source.lock().unwrap() = Some(source.clone());
        Ok(self.playback.lock().unwrap().take().unwrap())
    }
}

struct StubMediaFetcher {
    bytes: Vec<u8>,
}

#[async_trait]
impl MediaFetcher for StubMediaFetcher {
    async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
        Ok(MediaInfo {
            content_length: self.bytes.len() as u64,
            supports_ranges: true,
        })
    }

    async fn open_continuous(&self, _media: &PlayableMedia) -> Result<MediaBody, MediaFetchError> {
        let chunks = self
            .bytes
            .chunks(64 * 1024)
            .map(|chunk| Ok::<_, MediaFetchError>(Bytes::copy_from_slice(chunk)))
            .collect::<Vec<_>>();

        Ok(MediaBody {
            content_length: Some(self.bytes.len() as u64),
            chunks: Box::pin(tokio_stream::iter(chunks)),
        })
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

fn candidate(source: SourceIdentity) -> CatalogueCandidate {
    CatalogueCandidate::new(
        source,
        TrackMetadata::new(
            "Instant Crush".to_owned(),
            vec!["Daft Punk".to_owned()],
            Some(337_000),
        )
        .unwrap(),
    )
}

fn playable_media() -> PlayableMedia {
    PlayableMedia::new(
        PlaybackUrl::new("https://media.example.test/audio".to_owned()).unwrap(),
        HashMap::from([("User-Agent".to_owned(), "whio-test".to_owned())]),
        Some("2026-08-08T00:00:00Z".parse::<DateTime<Utc>>().unwrap()),
        MediaMetadata::new(
            Some("audio/webm".to_owned()),
            Some(1_024),
            Some("opus".to_owned()),
            Some(128.5),
            Some(337_250),
        )
        .unwrap(),
    )
}

fn app() -> (Router, Arc<StubResolver>) {
    app_with_media_bytes(b"0123456789".to_vec())
}

fn app_with_media_bytes(bytes: Vec<u8>) -> (Router, Arc<StubResolver>) {
    let source = source();
    let resolver = Arc::new(StubResolver {
        candidate: candidate(source),
        playback: Mutex::new(Some(playable_media())),
        resolved_source: Mutex::new(None),
    });
    let track_repository = Arc::new(InMemoryTrackRepository::default());
    let catalogue = Arc::new(CatalogueService::new(
        resolver.clone(),
        track_repository.clone(),
    ));
    let playback = Arc::new(PlaybackService::new(resolver.clone(), track_repository));
    let playback_stream = Arc::new(PlaybackStreamService::new(
        Arc::clone(&playback),
        Arc::new(StubMediaFetcher { bytes }),
    ));

    (
        router(credential_service(), catalogue, playback, playback_stream),
        resolver,
    )
}

fn request(uri: &str, body: String) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(body))
        .unwrap()
}

fn stream_request(uri: &str, range: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123");

    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }

    request.body(Body::empty()).unwrap()
}

async fn response_body(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test]
async fn searched_track_resolves_to_playable_media() {
    let (app, resolver) = app();
    let search_response = app
        .clone()
        .oneshot(request(
            "/catalogue/search",
            json!({"query": "Instant Crush", "limit": 5}).to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(search_response.status(), StatusCode::OK);
    let search_body = response_body(search_response).await;
    let track_id = search_body["results"][0]["id"].as_str().unwrap();

    let playback_response = app
        .oneshot(request(
            "/playback/resolve",
            json!({"track_id": track_id}).to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(playback_response.status(), StatusCode::OK);
    assert_eq!(playback_response.headers()["x-request-id"], "request-123");
    assert_eq!(
        response_body(playback_response).await,
        json!({
            "url": "https://media.example.test/audio",
            "headers": {"User-Agent": "whio-test"},
            "expires_at": "2026-08-08T00:00:00Z",
            "media": {
                "content_type": "audio/webm",
                "content_length_bytes": 1024,
                "codec": "opus",
                "bitrate_kbps": 128.5,
                "duration_ms": 337250
            }
        })
    );
    assert_eq!(
        resolver.resolved_source.lock().unwrap().as_ref(),
        Some(&source())
    );
}

#[tokio::test]
async fn unknown_track_id_returns_not_found() {
    let (app, resolver) = app();
    let response = app
        .oneshot(request(
            "/playback/resolve",
            json!({"track_id": TrackId::new().to_string()}).to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        response_body(response).await,
        json!({
            "code": "track_not_found",
            "message": "The track was not found",
            "request_id": "request-123"
        })
    );
    assert!(resolver.resolved_source.lock().unwrap().is_none());
}

#[tokio::test]
async fn invalid_track_id_returns_bad_request() {
    let (app, resolver) = app();
    let response = app
        .oneshot(request(
            "/playback/resolve",
            json!({"track_id": "not-a-track-id"}).to_string(),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        response_body(response).await,
        json!({
            "code": "invalid_request",
            "message": "The request is invalid",
            "request_id": "request-123"
        })
    );
    assert!(resolver.resolved_source.lock().unwrap().is_none());
}

async fn searched_track_id(app: &Router) -> String {
    let response = app
        .clone()
        .oneshot(request(
            "/catalogue/search",
            json!({"query": "Instant Crush", "limit": 5}).to_string(),
        ))
        .await
        .unwrap();

    response_body(response).await["results"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn stream_returns_full_media_without_a_range() {
    let (app, _) = app();
    let track_id = searched_track_id(&app).await;

    let response = app
        .oneshot(stream_request(
            &format!("/playback/tracks/{track_id}/stream"),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        "0123456789"
    );
}

#[tokio::test]
async fn stream_returns_requested_partial_media() {
    let (app, _) = app();
    let track_id = searched_track_id(&app).await;

    let response = app
        .oneshot(stream_request(
            &format!("/playback/tracks/{track_id}/stream"),
            Some("bytes=2-5"),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "4");
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
    assert_eq!(to_bytes(response.into_body(), 1024).await.unwrap(), "2345");
}

#[tokio::test]
async fn stream_returns_exact_bytes_for_large_media() {
    const TEST_CHUNK_SIZE: usize = 1024 * 1024;

    let bytes = (0..TEST_CHUNK_SIZE + 17)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let expected = bytes.clone();
    let (app, _) = app_with_media_bytes(bytes);
    let track_id = searched_track_id(&app).await;

    let response = app
        .oneshot(stream_request(
            &format!("/playback/tracks/{track_id}/stream"),
            None,
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        expected.len().to_string()
    );
    assert_eq!(
        to_bytes(response.into_body(), expected.len())
            .await
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn stream_returns_partial_bytes_for_large_media() {
    const TEST_CHUNK_SIZE: u64 = 1024 * 1024;

    let bytes = (0..TEST_CHUNK_SIZE * 2 + 17)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let start = TEST_CHUNK_SIZE - 5;
    let end = TEST_CHUNK_SIZE + 10;
    let expected = bytes[start as usize..=end as usize].to_vec();
    let (app, _) = app_with_media_bytes(bytes.clone());
    let track_id = searched_track_id(&app).await;

    let response = app
        .oneshot(stream_request(
            &format!("/playback/tracks/{track_id}/stream"),
            Some(&format!("bytes={start}-{end}")),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        expected.len().to_string()
    );
    assert_eq!(
        response.headers()[header::CONTENT_RANGE],
        format!("bytes {start}-{end}/{}", bytes.len())
    );
    assert_eq!(
        to_bytes(response.into_body(), expected.len())
            .await
            .unwrap(),
        expected
    );
}

#[tokio::test]
async fn stream_rejects_unsatisfiable_ranges() {
    let (app, _) = app();
    let track_id = searched_track_id(&app).await;

    let response = app
        .oneshot(stream_request(
            &format!("/playback/tracks/{track_id}/stream"),
            Some("bytes=10-"),
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */10");
    assert_eq!(
        response_body(response).await,
        json!({
            "code": "range_not_satisfiable",
            "message": "The requested range is not satisfiable",
            "request_id": "request-123"
        })
    );
}
