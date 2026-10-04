use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::path::PathBuf;
use std::sync::Arc;
use whio_core::features::music_library::test_support::PostgresTrackRepository;

use crate::support;

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use base64::Engine;
use base64::engine::general_purpose::STANDARD_NO_PAD;
use tower::ServiceExt;
use uuid::Uuid;

use support::credentials::{MemoryCredentialStore, TEST_PASSWORD};
use support::{NATIVE_AUTH_QUERY, StorePorts, router_with_artwork};
use whio_core::{
    features::identity::{
        domain::{User, UserId, UserRole},
        secrets::CredentialKey,
        service::CredentialService,
    },
    features::music_library::{
        ArtworkStore,
        artwork::{
            ArtworkAssetDescriptor, ArtworkCache, ArtworkRole, ArtworkService, ArtworkVariant,
        },
        domain::{ArtistId, ArtworkRef, ProviderId, SourceIdentity, SourceScope, TrackMetadata},
    },
};

fn credential_key() -> CredentialKey {
    CredentialKey::from_base64(&STANDARD_NO_PAD.encode([11u8; 32])).expect("valid test key")
}

fn credentials() -> (Arc<CredentialService>, Uuid, CredentialKey) {
    let key = credential_key();
    let store = Arc::new(MemoryCredentialStore::new());
    let user_id = UserId::from_uuid(Uuid::now_v7());
    let user_uuid = user_id.as_uuid();
    store.insert_user(User {
        id: user_id,
        username: "test-user".to_owned(),
        display_name: "Integration Test".to_owned(),
        role: UserRole::Member,
    });
    store.insert_app_password(&key, TEST_PASSWORD, user_id);

    (
        Arc::new(CredentialService::new(store, key.clone())),
        user_uuid,
        key,
    )
}

fn metadata() -> TrackMetadata {
    TrackMetadata::from_names("Song".to_owned(), vec!["Artist".to_owned()], Some(1_000)).unwrap()
}

async fn image_server(bytes: &'static [u8]) -> String {
    use axum::{Router, routing::get};

    let app = Router::new().route(
        "/cover.jpg",
        get(move || async move { ([(header::CONTENT_TYPE, "image/jpeg")], bytes) }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await });
    format!("http://{address}")
}

async fn public_app(pool: PgPool, url: &str) -> (axum::Router, String) {
    use whio_core::features::music_library::test_support::{CatalogueWrite, SourceLookup};

    let (credential, _user, _key) = credentials();
    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));

    let source = SourceIdentity::new(
        ProviderId::new("youtube_music".to_owned()).unwrap(),
        SourceScope::Global,
        "vid-1".to_owned(),
    )
    .unwrap();
    let track = repository
        .get_or_create(source.clone(), metadata())
        .await
        .unwrap();
    repository
        .set_source_artwork(&source, &ArtworkRef::url(url.to_owned()).unwrap())
        .await
        .unwrap();

    let ports = StorePorts { store: repository };
    let (search, library) = ports.bare_catalogue();
    let artwork = Arc::new(ArtworkService::new(
        ports.library(),
        ports.sources(),
        whio_core::features::music_library::artwork::default_host_suffixes(),
    ));
    let playback = ports.streaming(Vec::new());

    (
        router_with_artwork(credential, search, library, playback, artwork),
        track.id().to_string(),
    )
}

fn artwork_request(track_id: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!(
            "/playback/tracks/{track_id}/artwork{NATIVE_AUTH_QUERY}"
        ))
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap()
}

#[sqlx::test]
async fn unknown_and_malformed_track_ids_do_not_serve_artwork(pool: PgPool) {
    let base = image_server(b"cover-bytes").await;
    let (app, _track_id) = public_app(pool.clone(), &format!("{base}/cover.jpg")).await;

    let response = app
        .clone()
        .oneshot(artwork_request(&Uuid::now_v7().to_string()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .oneshot(artwork_request("not-a-track-id"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn public_artwork_outside_the_allowed_hosts_is_not_fetched(pool: PgPool) {
    let base = image_server(b"cover-bytes").await;
    let (app, track_id) = public_app(pool.clone(), &format!("{base}/cover.jpg")).await;

    let response = app.oneshot(artwork_request(&track_id)).await.unwrap();

    // the local http host is not an allowed https suffix, and the request never
    // reaches the image server
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn pictured_app(pool: PgPool) -> (axum::Router, String, String, String) {
    use whio_core::features::music_library::domain::{AlbumMetadata, AlbumTrack, ArtistCredit};
    use whio_core::features::music_library::test_support::{CatalogueWrite, SourceFileFacts};

    let (credential, _user, _key) = credentials();
    let root = std::env::temp_dir().join(format!("whio-http-art-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let file = support::pictured_flac(&root);

    let repository = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let source = SourceIdentity::new(
        ProviderId::new("local".to_owned()).unwrap(),
        SourceScope::Named(root.display().to_string()),
        "recording-1".to_owned(),
    )
    .unwrap();
    let track = repository
        .get_or_create(source.clone(), metadata())
        .await
        .unwrap();
    repository
        .set_source_file_facts(
            &source,
            &SourceFileFacts {
                path: Some("a.flac".to_owned()),
                size_bytes: std::fs::metadata(&file).unwrap().len(),
                modified_at: None,
                missing_at: None,
                content_type: None,
                suffix: Some("flac".to_owned()),
                container: None,
                codec: None,
                bitrate_kbps: None,
                sample_rate_hz: None,
                bit_depth: None,
                channels: None,
                facts_version: 0,
                acoustid_fingerprint: None,
            },
        )
        .await
        .unwrap();
    let artist = repository
        .resolve_or_create_artist(&ArtistCredit::new("Artist".to_owned(), None).unwrap())
        .await
        .unwrap();
    let album = repository
        .resolve_or_create_album(
            &AlbumMetadata::new("Album".to_owned())
                .unwrap()
                .with_album_artist(ArtistCredit::new("Artist".to_owned(), None).unwrap()),
        )
        .await
        .unwrap();
    repository
        .set_album_track(
            album.id(),
            track.id(),
            &AlbumTrack::new(Some(1), Some(1), 0).unwrap(),
        )
        .await
        .unwrap();

    let ports = StorePorts { store: repository };
    let (search, library) = ports.bare_catalogue();
    let artwork = Arc::new(ArtworkService::new(
        ports.library(),
        ports.sources(),
        whio_core::features::music_library::artwork::default_host_suffixes(),
    ));
    let playback = ports.streaming(Vec::new());

    (
        router_with_artwork(credential, search, library, playback, artwork),
        format!("al-{}", album.id()),
        format!("ar-{}", artist.id()),
        track.id().to_string(),
    )
}

fn subsonic_cover_art_request(id: &str) -> Request<Body> {
    Request::builder()
        .method("GET")
        .uri(format!(
            "/rest/getCoverArt.view?u=test-user&v=1.16.1&c=test&p={TEST_PASSWORD}&id={id}"
        ))
        .header("x-request-id", "request-123")
        .body(Body::empty())
        .unwrap()
}

fn sized_cover_art_request(id: &str, size: u32, validator: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder()
        .method("GET")
        .uri(format!(
            "/rest/getCoverArt.view?u=test-user&v=1.16.1&c=test&p={TEST_PASSWORD}&id={id}&size={size}"
        ))
        .header("x-request-id", "request-123");
    if let Some(validator) = validator {
        builder = builder.header(header::IF_NONE_MATCH, validator);
    }

    builder.body(Body::empty()).unwrap()
}

fn artist_jpeg(width: u32, height: u32) -> Vec<u8> {
    let image = image::RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, (y % 256) as u8, 96])
    });
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, image::ImageFormat::Jpeg)
        .unwrap();

    encoded.into_inner()
}

fn ready_asset(
    artist: &ArtistId,
    variant: ArtworkVariant,
    file_name: &str,
    bytes: &[u8],
) -> ArtworkAssetDescriptor {
    let size = variant.size().expect("a derived variant has a size");
    let mut hasher = Sha256::new();
    hasher.update(bytes);

    ArtworkAssetDescriptor {
        id: Uuid::now_v7(),
        subject: artist.clone(),
        role: ArtworkRole::Portrait,
        source_provider_id: "youtube_music".to_owned(),
        source_external_id: "channel-1".to_owned(),
        revision: 1,
        variant,
        content_digest: hex::encode(hasher.finalize()),
        content_type: "image/jpeg".to_owned(),
        width: size,
        height: size,
        byte_size: bytes.len() as i64,
        file_name: file_name.to_owned(),
    }
}

async fn ready_artist_app(pool: PgPool) -> (axum::Router, String, PathBuf) {
    let (credential, _user, _key) = credentials();
    let repository = Arc::new(PostgresTrackRepository::new(pool));
    let artist = ArtistId::new();
    let root = std::env::temp_dir().join(format!("whio-http-ready-art-{}", Uuid::now_v7()));
    std::fs::create_dir_all(&root).unwrap();
    let px256 = artist_jpeg(256, 256);
    let px1600 = artist_jpeg(1600, 1600);
    std::fs::write(root.join("px256.jpg"), &px256).unwrap();
    std::fs::write(root.join("px1600.jpg"), &px1600).unwrap();

    let assets = Arc::clone(&repository) as Arc<dyn ArtworkStore>;
    assets
        .record_assets(&[
            ready_asset(&artist, ArtworkVariant::Px256, "px256.jpg", &px256),
            ready_asset(&artist, ArtworkVariant::Px1600, "px1600.jpg", &px1600),
        ])
        .await
        .unwrap();

    let ports = StorePorts { store: repository };
    let (search, library) = ports.bare_catalogue();
    let artwork = Arc::new(
        ArtworkService::new(
            ports.library(),
            ports.sources(),
            whio_core::features::music_library::artwork::default_host_suffixes(),
        )
        .with_cache(Arc::new(ArtworkCache::new(
            assets,
            Some(root.clone()),
            u64::MAX,
        ))),
    );
    let playback = ports.streaming(Vec::new());

    (
        router_with_artwork(credential, search, library, playback, artwork),
        format!("ar-{}", artist.as_uuid()),
        root,
    )
}

#[sqlx::test]
async fn album_and_artist_ids_serve_embedded_art_with_validators(pool: PgPool) {
    let (app, album_id, artist_id, track_id) = pictured_app(pool.clone()).await;

    for id in [&album_id, &artist_id, &track_id] {
        let response = app
            .clone()
            .oneshot(subsonic_cover_art_request(id))
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "id: {id}");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "image/jpeg"
        );
        let etag = response
            .headers()
            .get(header::ETAG)
            .expect("etag")
            .to_str()
            .unwrap()
            .to_owned();
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "public, max-age=86400"
        );

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/rest/getCoverArt.view?u=test-user&v=1.16.1&c=test&p={TEST_PASSWORD}&id={id}"
                    ))
                    .header("x-request-id", "request-123")
                    .header(header::IF_NONE_MATCH, etag)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "id: {id}");
    }
}

#[sqlx::test]
async fn unknown_albums_and_malformed_ids_do_not_serve_artwork(pool: PgPool) {
    let (app, _, _, _) = pictured_app(pool.clone()).await;

    let response = app
        .clone()
        .oneshot(subsonic_cover_art_request(&format!(
            "al-{}",
            Uuid::now_v7()
        )))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let response = app
        .oneshot(subsonic_cover_art_request("al-not-a-uuid"))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
}

#[sqlx::test]
async fn a_sized_artist_portrait_is_served_from_ready_bytes_with_its_own_validator(pool: PgPool) {
    let (app, artist, root) = ready_artist_app(pool).await;
    let px256 = std::fs::read(root.join("px256.jpg")).unwrap();
    let px1600 = std::fs::read(root.join("px1600.jpg")).unwrap();

    let large = app
        .clone()
        .oneshot(sized_cover_art_request(&artist, 1200, None))
        .await
        .unwrap();
    let large_etag = large
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let large_bytes = axum::body::to_bytes(large.into_body(), usize::MAX)
        .await
        .unwrap();

    let small = app
        .clone()
        .oneshot(sized_cover_art_request(&artist, 64, Some(&large_etag)))
        .await
        .unwrap();
    assert_eq!(
        small.status(),
        StatusCode::OK,
        "a 64px request revalidated against the 1200px etag {large_etag}"
    );
    let small_etag = small
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let small_bytes = axum::body::to_bytes(small.into_body(), usize::MAX)
        .await
        .unwrap();

    assert_eq!(small_bytes.as_ref(), px256.as_slice());
    assert!(small_etag.ends_with("-w64\""), "{small_etag}");
    assert_ne!(large_etag, small_etag);

    assert_eq!(large_bytes.as_ref(), px1600.as_slice());
    assert!(large_etag.ends_with("-w1200\""), "{large_etag}");

    let revalidated = app
        .clone()
        .oneshot(sized_cover_art_request(&artist, 64, Some(&small_etag)))
        .await
        .unwrap();
    assert_eq!(revalidated.status(), StatusCode::NOT_MODIFIED);
    assert_eq!(
        revalidated.headers().get(header::ETAG).unwrap(),
        small_etag.as_str()
    );

    let large_revalidated = app
        .oneshot(sized_cover_art_request(&artist, 1200, Some(&large_etag)))
        .await
        .unwrap();
    assert_eq!(large_revalidated.status(), StatusCode::NOT_MODIFIED);
}
