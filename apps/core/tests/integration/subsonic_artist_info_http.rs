use crate::support;

use support::{StorePorts, credential_service_with_known_app_password, router};

use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
    response::Response,
};
use serde_json::{Value, json};
use sqlx::PgPool;
use tower::ServiceExt;
use whio_core::features::music_library::domain::{ArtistCredit, ArtistId};

fn client(pool: PgPool) -> (Router, String) {
    let (credential, secret) = credential_service_with_known_app_password();
    let ports = StorePorts::new(pool);
    let (search, library) = ports.bare_catalogue();
    let playback = ports.streaming(Vec::new());

    (router(credential, search, library, playback), secret)
}

fn auth_query(secret: &str) -> String {
    format!("u=flynn&v=1.16.1&c=test-client&p={secret}")
}

async fn send(app: &Router, path: &str, auth: &str, extra: &str) -> Response {
    let uri = if extra.is_empty() {
        format!("{path}?{auth}")
    } else {
        format!("{path}?{auth}&{extra}")
    };

    app.clone()
        .oneshot(
            Request::builder()
                .method("GET")
                .uri(uri)
                .header("x-request-id", support::TEST_REQUEST_ID)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

async fn body(response: Response) -> Value {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();

    serde_json::from_slice(&bytes).unwrap()
}

const MISSING_PARAMETER: (u16, &str) = (10, "Required parameter is missing.");
const NOT_FOUND: (u16, &str) = (70, "Not found.");
const INVALID_REQUEST: (u16, &str) = (0, "Invalid browse parameters.");
const INTERNAL_ERROR: (u16, &str) = (0, "Internal error.");

fn failure((code, message): (u16, &str)) -> Value {
    json!({
        "subsonic-response": {
            "status": "failed",
            "version": "1.16.1",
            "type": "whio",
            "serverVersion": "0.1.0",
            "openSubsonic": true,
            "error": {
                "code": code,
                "message": message,
            }
        }
    })
}

async fn artist(pool: &PgPool, name: &str, mbid: &str) -> ArtistId {
    StorePorts::new(pool.clone())
        .catalogue()
        .resolve_or_create_artist(
            &ArtistCredit::new(name.to_owned(), Some(mbid.to_owned())).unwrap(),
        )
        .await
        .unwrap()
        .id()
        .clone()
}

#[sqlx::test]
async fn artist_info_reports_a_missing_and_an_unknown_id(pool: PgPool) {
    let (app, secret) = client(pool);
    let auth = auth_query(&secret);

    for path in [
        "/rest/getArtistInfo.view",
        "/rest/getArtistInfo2.view",
        "/compat/subsonic/rest/getArtistInfo2.view",
    ] {
        let response = send(&app, path, &auth, "").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, failure(MISSING_PARAMETER));
    }

    let unknown = ArtistId::new().to_string();
    let response = send(
        &app,
        "/rest/getArtistInfo2.view",
        &auth,
        &format!("id={unknown}"),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body(response).await, failure(NOT_FOUND));

    let response = send(&app, "/rest/getArtistInfo2.view", &auth, "id=not-a-uuid").await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body(response).await, failure(NOT_FOUND));
}

#[sqlx::test]
async fn artist_info_rejects_a_bad_count_or_flag(pool: PgPool) {
    let (app, secret) = client(pool);
    let auth = auth_query(&secret);
    let id = ArtistId::new().to_string();

    for extra in [
        format!("id={id}&count=many"),
        format!("id={id}&count=-1"),
        format!("id={id}&includeNotPresent=maybe"),
    ] {
        for path in ["/rest/getArtistInfo.view", "/rest/getArtistInfo2.view"] {
            let response = send(&app, path, &auth, &extra).await;

            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(body(response).await, failure(INVALID_REQUEST));
        }
    }
}

#[sqlx::test]
async fn artist_info_records_a_disabled_metadata_feature_without_breaking_the_artist_routes(
    pool: PgPool,
) {
    let artist_id = artist(&pool, "Daft Punk", "mbid-daft-punk").await;
    let (app, secret) = client(pool);
    let auth = auth_query(&secret);
    let query = format!("id={artist_id}");
    let with_similar_parameters = format!("{query}&count=10&includeNotPresent=true");

    for (path, extra) in [
        ("/rest/getArtistInfo.view", &query),
        ("/rest/getArtistInfo2.view", &query),
        ("/rest/getArtistInfo2.view", &with_similar_parameters),
    ] {
        let response = send(&app, path, &auth, extra).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(body(response).await, failure(INTERNAL_ERROR));
    }

    let response = send(&app, "/rest/getArtist.view", &auth, &query).await;

    assert_eq!(response.status(), StatusCode::OK);
    let artist_body = body(response).await;
    assert_eq!(artist_body["subsonic-response"]["status"], "ok");
    assert_eq!(
        artist_body["subsonic-response"]["artist"]["id"],
        artist_id.to_string()
    );

    let response = send(&app, "/rest/getArtists.view", &auth, "").await;

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await["subsonic-response"]["status"], "ok");
}

#[sqlx::test]
async fn artist_info_needs_a_known_principal(pool: PgPool) {
    let (app, _secret) = client(pool);

    for path in ["/rest/getArtistInfo.view", "/rest/getArtistInfo2.view"] {
        let response = send(&app, path, "u=flynn&v=1.16.1&c=test-client", "").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, failure(MISSING_PARAMETER));
    }
}

mod subsonic_discovery {
    use super::*;

    use std::sync::Arc;

    use async_trait::async_trait;
    use whio_core::app::AppState;
    use whio_core::features::metadata::capabilities::{
        CAPABILITY_TTL, CapabilityError, CapabilitySnapshot, CapabilitySource, Snapshot,
    };
    use whio_core::features::metadata::domain::FieldId;
    use whio_core::features::metadata::freshness::NoJitter;
    use whio_core::features::metadata::refresh::{
        DocumentError, DocumentOutcome, MetadataDocuments,
    };
    use whio_core::features::metadata::selection::{FieldOrder, YOUTUBE_MUSIC};
    use whio_core::features::metadata::service::{MetadataProfile, MetadataService};
    use whio_core::features::metadata::{MetadataStore, PostgresMetadataStore};
    use whio_core::features::music_library::EnrichmentGuard;
    use whio_core::features::music_library::catalogue::LibraryService;
    use whio_core::features::music_library::discovery::DiscoveryService;
    use whio_core::features::music_library::domain::{
        AlbumId, AlbumMetadata, AlbumTrack, ArtistCredit, CreditRole, ProviderId, SourceIdentity,
        SourceScope, TrackId, TrackMetadata,
    };
    use whio_core::features::music_library::test_support::ListenStore;

    const SEED_MBID: &str = "mbid-http-seed";
    const PEER_MBID: &str = "mbid-http-peer";
    const QUIET_MBID: &str = "mbid-http-quiet";
    const MAX_METADATA_BYTES: u64 = 4 * 1024 * 1024;

    struct FixedCapabilities;

    #[async_trait]
    impl CapabilitySource for FixedCapabilities {
        async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
            Ok(Snapshot::new(vec![YOUTUBE_MUSIC.to_owned()]))
        }
    }

    struct NoDocuments;

    #[async_trait]
    impl MetadataDocuments for NoDocuments {
        async fn fetch(
            &self,
            _source: &SourceIdentity,
            _profile: &MetadataProfile,
            _language: &str,
            _fields: &[FieldId],
        ) -> Result<DocumentOutcome, DocumentError> {
            Err(DocumentError::Unsupported)
        }
    }

    struct Seeded {
        seed: ArtistId,
        peer: ArtistId,
        quiet: ArtistId,
        album: AlbumId,
        seed_track: TrackId,
        peer_track: TrackId,
    }

    fn credit(name: &str, mbid: &str) -> ArtistCredit {
        ArtistCredit::new(name.to_owned(), Some(mbid.to_owned())).expect("a credit")
    }

    async fn seed(pool: &PgPool) -> Seeded {
        let ports = StorePorts::new(pool.clone());
        let catalogue = ports.catalogue();
        let seed = catalogue
            .resolve_or_create_artist(&credit("Http Seed", SEED_MBID))
            .await
            .expect("the seed artist");
        let peer = catalogue
            .resolve_or_create_artist(&credit("Http Peer", PEER_MBID))
            .await
            .expect("the peer artist");
        let quiet = catalogue
            .resolve_or_create_artist(&credit("Http Quiet", QUIET_MBID))
            .await
            .expect("the quiet artist");
        for (id, mbid) in [
            (seed.id(), SEED_MBID),
            (peer.id(), PEER_MBID),
            (quiet.id(), QUIET_MBID),
        ] {
            catalogue
                .set_artist_source(id, &musicbrainz_identity(mbid))
                .await
                .expect("the artist source");
        }
        let album = catalogue
            .resolve_or_create_album(
                &AlbumMetadata::new("Http Seed Album".to_owned())
                    .expect("album metadata")
                    .with_album_artist(credit("Http Seed", SEED_MBID)),
            )
            .await
            .expect("the seed album");
        let peer_album = catalogue
            .resolve_or_create_album(
                &AlbumMetadata::new("Http Peer Album".to_owned())
                    .expect("album metadata")
                    .with_album_artist(credit("Http Peer", PEER_MBID)),
            )
            .await
            .expect("the peer album");
        let seed_track = catalogue
            .get_or_create(
                local_source("http-seed.flac"),
                TrackMetadata::new(
                    "Http Seed Song".to_owned(),
                    vec![credit("Http Seed", SEED_MBID)],
                    Some(1_000),
                )
                .expect("track metadata")
                .with_credits(vec![
                    credit("Http Seed", SEED_MBID).with_role(CreditRole::Composer),
                    credit("Http Peer", PEER_MBID).with_role(CreditRole::Composer),
                ]),
            )
            .await
            .expect("the seed track")
            .id()
            .clone();
        let peer_track = catalogue
            .get_or_create(
                local_source("http-peer.flac"),
                TrackMetadata::new(
                    "Http Peer Song".to_owned(),
                    vec![credit("Http Peer", PEER_MBID)],
                    Some(2_000),
                )
                .expect("track metadata"),
            )
            .await
            .expect("the peer track")
            .id()
            .clone();
        catalogue
            .set_album_track(
                album.id(),
                &seed_track,
                &AlbumTrack::new(Some(1), Some(1), 0).expect("a membership"),
            )
            .await
            .expect("the seed membership");
        catalogue
            .set_album_track(
                peer_album.id(),
                &peer_track,
                &AlbumTrack::new(Some(1), Some(1), 0).expect("a membership"),
            )
            .await
            .expect("the peer membership");

        Seeded {
            seed: seed.id().clone(),
            peer: peer.id().clone(),
            quiet: quiet.id().clone(),
            album: album.id().clone(),
            seed_track,
            peer_track,
        }
    }

    fn musicbrainz_identity(mbid: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("musicbrainz".to_owned()).expect("a provider"),
            SourceScope::Global,
            mbid.to_owned(),
        )
        .expect("a source identity")
    }

    fn local_source(external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new("local".to_owned()).expect("a provider"),
            SourceScope::Named("/music".to_owned()),
            external_id.to_owned(),
        )
        .expect("a source identity")
    }

    fn client_with_discovery(pool: PgPool) -> (Router, String) {
        let (credential, secret) = credential_service_with_known_app_password();
        let ports = StorePorts::new(pool);
        let (search, library) = ports.bare_catalogue();
        let playback = ports.streaming(Vec::new());
        let discovery = Arc::new(DiscoveryService::new(
            ports.library(),
            ports.sources(),
            ports.state(),
            Arc::clone(&ports.store) as Arc<dyn ListenStore>,
        ));
        let state = AppState::new(credential, search, library, playback).with_discovery(discovery);

        (whio_core::app::router(state), secret)
    }

    async fn client_with_metadata(pool: PgPool) -> (Router, String) {
        let (credential, secret) = credential_service_with_known_app_password();
        awa::migrations::run(&pool).await.expect("the awa schema");
        let ports = StorePorts::new(pool.clone());
        let (search, _) = ports.bare_catalogue();
        let playback = ports.streaming(Vec::new());
        let discovery = Arc::new(DiscoveryService::new(
            ports.library(),
            ports.sources(),
            ports.state(),
            Arc::clone(&ports.store) as Arc<dyn ListenStore>,
        ));
        let snapshot = Arc::new(CapabilitySnapshot::new(
            Arc::new(FixedCapabilities),
            CAPABILITY_TTL,
        ));
        let store = Arc::new(PostgresMetadataStore::new(
            pool.clone(),
            Arc::clone(&ports.store) as Arc<dyn EnrichmentGuard>,
            MAX_METADATA_BYTES,
        ));
        let metadata = Arc::new(MetadataService::new(
            pool,
            Arc::clone(&store) as Arc<dyn MetadataStore>,
            Arc::new(NoDocuments) as Arc<dyn MetadataDocuments>,
            snapshot,
            FieldOrder::from_entries([
                (FieldId::Similar, vec![YOUTUBE_MUSIC.to_owned()]),
                (FieldId::TopTracks, vec![YOUTUBE_MUSIC.to_owned()]),
            ]),
            "v1".to_owned(),
            Arc::new(NoJitter),
        ));
        let library = Arc::new(
            LibraryService::new(ports.library(), ports.sources(), ports.state())
                .with_metadata(Arc::clone(&metadata)),
        );
        let state = AppState::new(credential, search, library, playback)
            .with_discovery(discovery)
            .with_metadata_service(metadata);

        (whio_core::app::router(state), secret)
    }

    fn song_ids(body: &Value, list: &str) -> Vec<String> {
        body["subsonic-response"][list]["song"]
            .as_array()
            .expect("the declared song list")
            .iter()
            .map(|song| song["id"].as_str().expect("a song id").to_owned())
            .collect()
    }

    #[sqlx::test]
    async fn top_songs_returns_the_local_tracks_of_an_exact_name_capped_at_count(pool: PgPool) {
        let data = seed(&pool).await;
        let (app, secret) = client_with_discovery(pool);
        let auth = auth_query(&secret);

        let response = send(
            &app,
            "/rest/getTopSongs.view",
            &auth,
            "artist=Http%20Seed&count=10",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let mut ids = song_ids(&body(response).await, "topSongs");
        ids.sort();

        assert_eq!(ids, vec![data.seed_track.to_string()]);

        let response = send(
            &app,
            "/rest/getTopSongs.view",
            &auth,
            "artist=Http%20Seed&count=1",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(song_ids(&body(response).await, "topSongs").len(), 1);
    }

    #[sqlx::test]
    async fn top_songs_maps_a_missing_and_an_unmatched_artist(pool: PgPool) {
        let data = seed(&pool).await;
        let (app, secret) = client_with_discovery(pool);
        let auth = auth_query(&secret);

        let response = send(&app, "/rest/getTopSongs.view", &auth, "").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, failure(MISSING_PARAMETER));

        let response = send(&app, "/rest/getTopSongs.view", &auth, "artist=Nobody").await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body(response).await, failure(NOT_FOUND));

        let response = send(
            &app,
            "/rest/getTopSongs.view",
            &auth,
            "artist=Http%20Quiet&count=50",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let absence = body(response).await;
        assert_eq!(absence["subsonic-response"]["topSongs"]["song"], json!([]));
        assert_ne!(data.quiet, data.seed);

        let response = send(
            &app,
            "/rest/getTopSongs.view",
            &auth,
            "artist=Http%20Seed&count=many",
        )
        .await;

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(body(response).await, failure(INVALID_REQUEST));
    }

    #[sqlx::test]
    async fn similar_songs_serve_the_local_relations_and_declare_an_empty_list(pool: PgPool) {
        let data = seed(&pool).await;
        let (app, secret) = client_with_discovery(pool);
        let auth = auth_query(&secret);

        let response = send(
            &app,
            "/rest/getSimilarSongs2.view",
            &auth,
            &format!("id={}&count=50", data.seed),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let mut ids = song_ids(&body(response).await, "similarSongs2");
        ids.sort();

        let mut expected = vec![data.seed_track.to_string(), data.peer_track.to_string()];
        expected.sort();
        assert_eq!(ids, expected);

        let response = send(
            &app,
            "/rest/getSimilarSongs.view",
            &auth,
            &format!("id={}&count=50", data.seed_track),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            song_ids(&body(response).await, "similarSongs"),
            vec![data.peer_track.to_string()],
            "the seed song is excluded"
        );

        let response = send(
            &app,
            "/rest/getSimilarSongs.view",
            &auth,
            &format!("id={}&count=50", data.album),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let mut ids = song_ids(&body(response).await, "similarSongs");
        ids.sort();

        let mut expected = vec![data.seed_track.to_string(), data.peer_track.to_string()];
        expected.sort();
        assert_eq!(ids, expected, "album radio is anchored on the album artist");

        let response = send(
            &app,
            "/rest/getSimilarSongs2.view",
            &auth,
            &format!("id={}&count=50", data.quiet),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body(response).await["subsonic-response"]["similarSongs2"]["song"],
            json!([])
        );

        let response = send(
            &app,
            "/rest/getSimilarSongs2.view",
            &auth,
            &format!("id={}", ArtistId::new()),
        )
        .await;

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(body(response).await, failure(NOT_FOUND));

        let response = send(&app, "/rest/getSimilarSongs.view", &auth, "").await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body(response).await, failure(MISSING_PARAMETER));
    }

    #[sqlx::test]
    async fn similar_artist_carries_the_canonical_peer_and_include_not_present_adds_nothing(
        pool: PgPool,
    ) {
        let data = seed(&pool).await;
        let (app, secret) = client_with_metadata(pool).await;
        let auth = auth_query(&secret);
        let query = format!("id={}", data.seed);

        let response = send(&app, "/rest/getArtistInfo2.view", &auth, &query).await;

        assert_eq!(response.status(), StatusCode::OK);
        let plain = body(response).await;
        let info = &plain["subsonic-response"]["artistInfo2"];
        assert_eq!(
            info["similarArtist"]
                .as_array()
                .expect("the related list")
                .len(),
            1
        );
        assert_eq!(info["similarArtist"][0]["id"], data.peer.to_string());
        assert_eq!(info["similarArtist"][0]["name"], "Http Peer");
        assert_eq!(info["similarArtist"][0]["albumCount"], 1);
        assert_eq!(
            info["similarArtist"][0]["coverArt"],
            format!("ar-{}", data.peer)
        );
        assert_eq!(info["largeImageUrl"], format!("ar-{}", data.seed));
        assert!(info.get("biography").is_none());

        let response = send(
            &app,
            "/rest/getArtistInfo2.view",
            &auth,
            &format!("{query}&includeNotPresent=true&count=20"),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            body(response).await,
            plain,
            "includeNotPresent adds nothing and still succeeds"
        );

        let response = send(
            &app,
            "/rest/getArtistInfo2.view",
            &auth,
            &format!("id={}&count=1", data.seed),
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let capped = body(response).await;
        assert_eq!(
            capped["subsonic-response"]["artistInfo2"]["similarArtist"]
                .as_array()
                .expect("the related list")
                .len(),
            1
        );
    }
}
