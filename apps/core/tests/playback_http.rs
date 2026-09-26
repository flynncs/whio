#![cfg(feature = "postgres-integration")]

use sqlx::PgPool;
use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;

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
use support::{NATIVE_AUTH_QUERY, StorePorts, credential_service, router};
use tower::ServiceExt;
use uuid::Uuid;
use whio_core::{
    features::music_library::catalogue::{
        CatalogueCandidate, CatalogueProvider, CatalogueSearch,
        ProviderError as CatalogueProviderError,
    },
    features::music_library::domain::{
        ProviderId, ProviderKind, SourceIdentity, SourceScope, TrackId, TrackMetadata,
    },
    features::playback::delivery::StreamDelivery,
    features::playback::media::{MediaBody, MediaFetchError, MediaFetcher, MediaInfo},
    features::playback::providers::ProviderError,
    features::playback::{
        MediaMetadata, PlayableMedia, PlaybackProvider, PlaybackSource, PlaybackUrl,
        ResolvedPlayback, StreamingService,
    },
    platform::request::RequestContext,
};

struct StubProvider {
    candidate: CatalogueCandidate,
    playback: Mutex<Option<PlayableMedia>>,
    resolved_source: Mutex<Option<PlaybackSource>>,
}

#[async_trait]
impl CatalogueProvider for StubProvider {
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, CatalogueProviderError> {
        Ok(vec![self.candidate.clone()])
    }
}

#[async_trait]
impl PlaybackProvider for StubProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::YoutubeMusic
    }

    async fn resolve(
        &self,
        source: &PlaybackSource,
        _user: Option<&Uuid>,
        _context: &RequestContext,
    ) -> Result<Option<ResolvedPlayback>, ProviderError> {
        *self.resolved_source.lock() = Some(source.clone());
        Ok(self.playback.lock().take().map(ResolvedPlayback::Remote))
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
            content_type: Some("audio/mpeg".to_owned()),
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
        TrackMetadata::from_names(
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

fn app(pool: PgPool) -> (Router, Arc<StubProvider>) {
    app_with_media_bytes(pool, b"0123456789".to_vec())
}

fn app_with_media_bytes(pool: PgPool, bytes: Vec<u8>) -> (Router, Arc<StubProvider>) {
    let source = source();
    let provider = Arc::new(StubProvider {
        candidate: candidate(source),
        playback: Mutex::new(Some(playable_media())),
        resolved_source: Mutex::new(None),
    });
    let ports = StorePorts::new(pool.clone());
    let (search, library) =
        ports.catalogue_service(vec![provider.clone() as Arc<dyn CatalogueProvider>]);
    let playback = Arc::new(StreamingService::new(
        ports.source_queries(),
        vec![provider.clone() as Arc<dyn PlaybackProvider>],
        Arc::new(StreamDelivery::new(
            Arc::new(StubMediaFetcher { bytes }),
            std::env::temp_dir(),
            whio_core::features::playback::delivery::DEFAULT_MAX_SESSIONS,
        )),
    ));

    (
        router(credential_service(), search, library, playback),
        provider,
    )
}

fn request(uri: &str, body: String) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(format!("{uri}{NATIVE_AUTH_QUERY}"))
        .header("content-type", "application/json")
        .header("x-request-id", "request-123")
        .body(Body::from(body))
        .unwrap()
}

fn stream_request(uri: &str, range: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .method("GET")
        .uri(format!("{uri}{NATIVE_AUTH_QUERY}"))
        .header("x-request-id", "request-123");

    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }

    request.body(Body::empty()).unwrap()
}

fn unauthenticated_stream_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap()
}

async fn response_body(response: axum::response::Response) -> Value {
    let body = to_bytes(response.into_body(), 8192).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[sqlx::test]
async fn unknown_track_id_returns_not_found(pool: PgPool) {
    let (app, resolver) = app(pool.clone());
    let response = app
        .oneshot(stream_request(
            &format!("/playback/tracks/{}/stream", TrackId::new()),
            None,
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
    assert!(resolver.resolved_source.lock().is_none());
}

#[sqlx::test]
async fn stream_requires_authentication(pool: PgPool) {
    let (app, _) = app(pool.clone());
    let response = app
        .oneshot(unauthenticated_stream_request(
            "/playback/tracks/not-a-track-id/stream",
        ))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[sqlx::test]
async fn invalid_track_id_returns_bad_request(pool: PgPool) {
    let (app, resolver) = app(pool.clone());
    let response = app
        .oneshot(stream_request(
            "/playback/tracks/not-a-track-id/stream",
            None,
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
    assert!(resolver.resolved_source.lock().is_none());
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

#[sqlx::test]
async fn stream_returns_full_media_without_a_range(pool: PgPool) {
    let (app, _) = app(pool.clone());
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

#[sqlx::test]
async fn stream_returns_requested_partial_media(pool: PgPool) {
    let (app, _) = app(pool.clone());
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

#[sqlx::test]
async fn stream_returns_exact_bytes_for_large_media(pool: PgPool) {
    const TEST_CHUNK_SIZE: usize = 1024 * 1024;

    let bytes = (0..TEST_CHUNK_SIZE + 17)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let expected = bytes.clone();
    let (app, _) = app_with_media_bytes(pool.clone(), bytes);
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

#[sqlx::test]
async fn stream_returns_partial_bytes_for_large_media(pool: PgPool) {
    const TEST_CHUNK_SIZE: u64 = 1024 * 1024;

    let bytes = (0..TEST_CHUNK_SIZE * 2 + 17)
        .map(|index| (index % 251) as u8)
        .collect::<Vec<_>>();
    let start = TEST_CHUNK_SIZE - 5;
    let end = TEST_CHUNK_SIZE + 10;
    let expected = bytes[start as usize..=end as usize].to_vec();
    let (app, _) = app_with_media_bytes(pool.clone(), bytes.clone());
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

#[sqlx::test]
async fn stream_rejects_unsatisfiable_ranges(pool: PgPool) {
    let (app, _) = app(pool.clone());
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
