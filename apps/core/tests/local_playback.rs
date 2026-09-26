#![cfg(feature = "postgres-integration")]

use sqlx::PgPool;
use whio_core::features::music_library::test_support::PostgresTrackRepository;
mod support;

use std::{path::PathBuf, sync::Arc};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode, header},
};
use support::{NATIVE_AUTH_QUERY, StorePorts, credential_service, router};
use tower::ServiceExt;
use whio_core::{
    features::music_library::domain::{
        ProviderId, SourceIdentity, SourceScope, TrackId, TrackMetadata,
    },
    features::music_library::test_support::{CatalogueWrite, SourceFileFacts},
    features::playback::PlaybackProvider,
    features::playback::providers::LocalPlaybackProvider,
};

const AUDIO_BYTES: &[u8] = b"local playback fixture bytes";

fn fixture_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("whio-local-http-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(root.join("Artist/Album")).unwrap();
    std::fs::write(root.join("Artist/Album/01.flac"), AUDIO_BYTES).unwrap();
    root
}

async fn app(pool: PgPool) -> (Router, TrackId, PathBuf) {
    let root = fixture_root();
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let source = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named(root.display().to_string()),
        "11111111-2222-3333-4444-555555555555".to_owned(),
    )
    .unwrap();
    let track = repository
        .get_or_create(
            source.clone(),
            TrackMetadata::from_names(
                "Fixture Song".to_owned(),
                vec!["Fixture Artist".to_owned()],
                Some(1_000),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    repository
        .set_source_file_facts(
            &source,
            &SourceFileFacts {
                path: Some("Artist/Album/01.flac".to_owned()),
                size_bytes: AUDIO_BYTES.len() as u64,
                modified_at: None,
                missing_at: None,
                content_type: None,
                suffix: Some("flac".to_owned()),
                acoustid_fingerprint: None,
            },
        )
        .await
        .unwrap();

    let ports = StorePorts { store: repository };
    let (search, library) = ports.catalogue_service(vec![]);
    let playback = ports.streaming(vec![
        Arc::new(LocalPlaybackProvider::new(ports.source_queries())) as Arc<dyn PlaybackProvider>,
    ]);

    (
        router(credential_service(), search, library, playback),
        track.id().clone(),
        root,
    )
}

fn stream_request(track_id: &TrackId, range: Option<&str>) -> Request<Body> {
    let mut request = Request::builder()
        .method("GET")
        .uri(format!(
            "/playback/tracks/{track_id}/stream{NATIVE_AUTH_QUERY}"
        ))
        .header("x-request-id", "request-123");

    if let Some(range) = range {
        request = request.header(header::RANGE, range);
    }

    request.body(Body::empty()).unwrap()
}

#[sqlx::test]
async fn a_local_track_streams_its_exact_bytes(pool: PgPool) {
    let (app, track_id, root) = app(pool.clone()).await;

    let response = app.oneshot(stream_request(&track_id, None)).await.unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::ACCEPT_RANGES], "bytes");
    assert_eq!(response.headers()[header::CONTENT_TYPE], "audio/flac");
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        AUDIO_BYTES.len().to_string()
    );
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        AUDIO_BYTES
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn a_local_track_serves_a_client_range(pool: PgPool) {
    let (app, track_id, root) = app(pool.clone()).await;

    let response = app
        .oneshot(stream_request(&track_id, Some("bytes=6-14")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        response.headers()[header::CONTENT_RANGE],
        format!("bytes 6-14/{}", AUDIO_BYTES.len())
    );
    assert_eq!(
        to_bytes(response.into_body(), 1024).await.unwrap(),
        &AUDIO_BYTES[6..=14]
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn a_local_track_rejects_an_unsatisfiable_range(pool: PgPool) {
    let (app, track_id, root) = app(pool.clone()).await;

    let response = app
        .oneshot(stream_request(&track_id, Some("bytes=999-")))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(
        response.headers()[header::CONTENT_RANGE],
        format!("bytes */{}", AUDIO_BYTES.len())
    );

    std::fs::remove_dir_all(root).unwrap();
}

#[sqlx::test]
async fn a_vanished_local_file_does_not_stream(pool: PgPool) {
    let (app, track_id, root) = app(pool.clone()).await;
    std::fs::remove_dir_all(&root).unwrap();

    let response = app.oneshot(stream_request(&track_id, None)).await.unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}
