use sqlx::PgPool;
use std::{collections::HashMap, sync::Arc};

use parking_lot::Mutex;

use crate::support;
use async_trait::async_trait;

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
    features::playback::delivery::{DEFAULT_MAX_SESSIONS, StreamDelivery},
    features::playback::media::{
        FfmpegConverter, MediaBody, MediaFetchError, MediaFetcher, MediaInfo,
    },
    features::playback::providers::ProviderError,
    features::playback::{
        ClientFormats, MediaMetadata, PlayableMedia, PlaybackProvider, PlaybackSource, PlaybackUrl,
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
    async fn search_tracks(
        &self,
        _search: &CatalogueSearch,
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
    content_type: &'static str,
}

#[async_trait]
impl MediaFetcher for StubMediaFetcher {
    async fn probe(&self, _media: &PlayableMedia) -> Result<MediaInfo, MediaFetchError> {
        Ok(MediaInfo {
            content_length: self.bytes.len() as u64,
            supports_ranges: true,
            content_type: Some(self.content_type.to_owned()),
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

fn conversion_media() -> PlayableMedia {
    PlayableMedia::new(
        PlaybackUrl::new("https://media.example.test/audio".to_owned()).unwrap(),
        HashMap::from([("User-Agent".to_owned(), "whio-test".to_owned())]),
        None,
        MediaMetadata::new(
            Some("audio/flac".to_owned()),
            Some(23_710),
            Some("flac".to_owned()),
            Some(900.0),
            Some(1_000),
        )
        .unwrap(),
    )
}

async fn app_with_conversion(pool: PgPool, bytes: Vec<u8>, formats: &str) -> Router {
    let source = source();
    let provider = Arc::new(StubProvider {
        candidate: candidate(source),
        playback: Mutex::new(Some(conversion_media())),
        resolved_source: Mutex::new(None),
    });
    let ports = StorePorts::new(pool.clone());
    let (search, library) =
        ports.catalogue_service(vec![provider.clone() as Arc<dyn CatalogueProvider>]);
    let converter = Arc::new(FfmpegConverter::probe(std::path::PathBuf::from("ffmpeg"), 2).await);
    let delivery = StreamDelivery::new(
        Arc::new(StubMediaFetcher {
            bytes,
            content_type: "audio/flac",
        }),
        std::env::temp_dir(),
        DEFAULT_MAX_SESSIONS,
    )
    .with_converter(converter)
    .with_client_formats(ClientFormats::parse(formats).unwrap());
    let playback = Arc::new(StreamingService::new(
        ports.source_queries(),
        vec![provider as Arc<dyn PlaybackProvider>],
        Arc::new(delivery),
    ));

    router(credential_service(), search, library, playback)
}

fn app_with_media_bytes(pool: PgPool, bytes: Vec<u8>) -> (Router, Arc<StubProvider>) {
    app_with_upstream_media(pool, bytes, "audio/mpeg")
}

fn app_with_upstream_media(
    pool: PgPool,
    bytes: Vec<u8>,
    content_type: &'static str,
) -> (Router, Arc<StubProvider>) {
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
            Arc::new(StubMediaFetcher {
                bytes,
                content_type,
            }),
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
        .header("x-request-id", support::TEST_REQUEST_ID)
        .body(Body::from(body))
        .unwrap()
}

fn stream_request(uri: &str, range: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .method("GET")
        .uri(format!("{uri}{NATIVE_AUTH_QUERY}"))
        .header("x-request-id", support::TEST_REQUEST_ID);

    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }

    request.body(Body::empty()).unwrap()
}

fn unauthenticated_stream_request(uri: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(uri)
        .header("x-request-id", support::TEST_REQUEST_ID)
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
            "request_id": support::TEST_REQUEST_ID
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
            "request_id": support::TEST_REQUEST_ID
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
            "request_id": support::TEST_REQUEST_ID
        })
    );
}

#[sqlx::test]
async fn provider_song_sentinel_does_not_replace_delivered_media_type(pool: PgPool) {
    for content_type in ["audio/webm", "audio/mp4"] {
        let (app, resolver) =
            app_with_upstream_media(pool.clone(), b"controlled media".to_vec(), content_type);
        let track_id = searched_track_id(&app).await;
        let detail_uri =
            format!("/compat/subsonic/rest/getSong.view{NATIVE_AUTH_QUERY}&id={track_id}");
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(&detail_uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::ACCESS_CONTROL_EXPOSE_HEADERS],
            "x-total-count"
        );
        let detail = response_body(response).await;
        let song = &detail["subsonic-response"]["song"];
        assert_eq!(song["contentType"], "application/octet-stream");
        for field in [
            "suffix",
            "size",
            "bitRate",
            "samplingRate",
            "bitDepth",
            "channelCount",
        ] {
            assert!(song.get(field).is_none(), "unexpected {field}");
        }
        assert!(resolver.resolved_source.lock().is_none());
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/compat/subsonic/rest/stream.view{NATIVE_AUTH_QUERY}&id={track_id}"
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
        assert_eq!(
            to_bytes(response.into_body(), 1024).await.unwrap(),
            "controlled media"
        );
        assert!(resolver.resolved_source.lock().is_some());
        let response = app
            .oneshot(
                Request::builder()
                    .uri(&detail_uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response_body(response).await["subsonic-response"]["song"]["contentType"],
            "application/octet-stream"
        );
    }
}

fn subsonic_stream_request(track_id: &str, client: &str, parameters: &str) -> Request<Body> {
    Request::builder()
            .uri(format!(
                "/compat/subsonic/rest/stream.view?u=test-user&v=1.16.1&c={client}&p=whio-test-password&id={track_id}{parameters}"
            ))
            .header("x-request-id", support::TEST_REQUEST_ID)
            .body(Body::empty())
            .unwrap()
}

#[sqlx::test]
async fn client_format_rules_decide_the_streamed_representation(pool: PgPool) {
    let bytes = std::fs::read(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/lossless.flac"),
    )
    .unwrap();
    let app = app_with_conversion(pool.clone(), bytes.clone(), "Arpeggi=aac").await;
    let track_id = searched_track_id(&app).await;

    let converted = app
        .clone()
        .oneshot(subsonic_stream_request(&track_id, "Arpeggi", ""))
        .await
        .unwrap();
    assert_eq!(converted.status(), StatusCode::OK);
    assert_eq!(converted.headers()[header::CONTENT_TYPE], "audio/aac");
    let converted = to_bytes(converted.into_body(), 1024 * 1024).await.unwrap();
    assert_eq!(converted[0], 0xFF);
    assert_eq!(converted[1] & 0xF0, 0xF0);

    let original = app
        .clone()
        .oneshot(subsonic_stream_request(&track_id, "Feishin", ""))
        .await
        .unwrap();
    assert_eq!(original.status(), StatusCode::OK);
    assert_eq!(original.headers()[header::CONTENT_TYPE], "audio/flac");
    assert_eq!(
        to_bytes(original.into_body(), 1024 * 1024)
            .await
            .unwrap()
            .len(),
        bytes.len()
    );

    let raw = app
        .oneshot(subsonic_stream_request(&track_id, "Arpeggi", "&format=raw"))
        .await
        .unwrap();
    assert_eq!(raw.status(), StatusCode::OK);
    assert_eq!(raw.headers()[header::CONTENT_TYPE], "audio/flac");
}
