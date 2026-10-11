use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use reqwest::Url;

use crate::features::acquisition::ports::{LookupError, MbId, MusicBrainzLookup};
use crate::platform::pacing::CallLimiter;

use super::client::{MusicBrainzClient, WikipediaHosts};

const RELEASE_LOOKUP: &str = include_str!("fixtures/controlled_musicbrainz_release_lookup.json");
const RELEASE_SEARCH: &str = include_str!("fixtures/controlled_musicbrainz_release_search.json");
const RECORDING_SEARCH: &str =
    include_str!("fixtures/controlled_musicbrainz_recording_search.json");

type Handler = Arc<dyn Fn(&str) -> (StatusCode, String) + Send + Sync>;

#[derive(Clone)]
struct Stub {
    records: Arc<Mutex<Vec<String>>>,
    handler: Handler,
}

async fn start(handler: Handler) -> (Url, Arc<Mutex<Vec<String>>>) {
    let records = Arc::new(Mutex::new(Vec::new()));
    let stub = Stub {
        records: Arc::clone(&records),
        handler,
    };
    let app = Router::new().fallback(any(respond)).with_state(stub);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    (Url::parse(&format!("http://{address}/")).unwrap(), records)
}

async fn respond(State(stub): State<Stub>, request: Request<Body>) -> Response<Body> {
    let uri = request.uri().to_string();
    stub.records.lock().unwrap().push(uri.clone());
    let (status, body) = (stub.handler)(&uri);

    (status, body).into_response()
}

fn static_handler(replies: Vec<(&'static str, StatusCode, &'static str)>) -> Handler {
    let replies = Arc::new(replies);

    Arc::new(move |uri| {
        replies
            .iter()
            .find(|(pattern, _, _)| uri.contains(pattern))
            .map(|(_, status, body)| (*status, (*body).to_owned()))
            .unwrap_or((StatusCode::NOT_FOUND, String::new()))
    })
}

fn client(base: &Url, limiter: Arc<CallLimiter>) -> MusicBrainzClient {
    MusicBrainzClient::with_hosts(
        base.clone(),
        Url::parse("http://127.0.0.1:1/").unwrap(),
        Url::parse("http://127.0.0.1:1/").unwrap(),
        WikipediaHosts::Fixed(Url::parse("http://127.0.0.1:1/").unwrap()),
        "agent".to_owned(),
        "agent".to_owned(),
        limiter,
    )
    .unwrap()
}

fn release_id() -> MbId {
    MbId::parse("11111111-1111-1111-1111-111111111111").unwrap()
}

#[tokio::test]
async fn a_release_lookup_carries_tracks_in_disc_order() {
    let (base, _) = start(static_handler(vec![(
        "/ws/2/release/",
        StatusCode::OK,
        RELEASE_LOOKUP,
    )]))
    .await;
    let release = client(&base, Arc::new(CallLimiter::new(Duration::ZERO)))
        .release(&release_id())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(release.artist, "An Artist");
    assert_eq!(release.title, "An Album");
    assert_eq!(
        release
            .tracks
            .iter()
            .map(|track| (track.disc, track.position, track.title.as_str()))
            .collect::<Vec<_>>(),
        vec![(1, 1, "One"), (1, 2, "Two"), (2, 1, "Second Disc")]
    );
    assert_eq!(release.tracks[0].length_ms, Some(120_000));
}

#[tokio::test]
async fn release_candidates_are_capped_at_five() {
    let (base, records) = start(static_handler(vec![
        ("query=", StatusCode::OK, RELEASE_SEARCH),
        ("/ws/2/release/", StatusCode::OK, RELEASE_LOOKUP),
    ]))
    .await;
    let releases = client(&base, Arc::new(CallLimiter::new(Duration::ZERO)))
        .release_candidates("An Artist", "An Album")
        .await
        .unwrap();

    assert_eq!(releases.len(), 5);
    let records = records.lock().unwrap();
    let lookups = records
        .iter()
        .filter(|uri| uri.contains("/ws/2/release/"))
        .count();
    assert_eq!(lookups, 5);
    assert!(!records.iter().any(|uri| uri.contains("000006")));
}

#[tokio::test]
async fn a_missing_release_is_none() {
    let (base, _) = start(static_handler(vec![(
        "/ws/2/release/",
        StatusCode::NOT_FOUND,
        "",
    )]))
    .await;
    let client = client(&base, Arc::new(CallLimiter::new(Duration::ZERO)));

    assert_eq!(client.release(&release_id()).await.unwrap(), None);
}

#[tokio::test]
async fn an_outage_is_unavailable() {
    let (base, _) = start(static_handler(vec![(
        "/ws/2/release/",
        StatusCode::SERVICE_UNAVAILABLE,
        "",
    )]))
    .await;
    let client = client(&base, Arc::new(CallLimiter::new(Duration::ZERO)));

    assert_eq!(
        client.release(&release_id()).await.unwrap_err(),
        LookupError::Unavailable
    );
}

#[tokio::test]
async fn recording_candidates_carry_length_and_artist() {
    let (base, _) = start(static_handler(vec![(
        "query=",
        StatusCode::OK,
        RECORDING_SEARCH,
    )]))
    .await;
    let recordings = client(&base, Arc::new(CallLimiter::new(Duration::ZERO)))
        .recording_candidates("An Artist", "A Track")
        .await
        .unwrap();

    assert_eq!(recordings.len(), 2);
    assert_eq!(recordings[0].title, "A Track");
    assert_eq!(recordings[0].artist, "An Artist");
    assert_eq!(recordings[0].length_ms, Some(210_000));
}

#[tokio::test]
async fn calls_share_the_document_limiter() {
    let instant = Instant::now();
    let limiter = Arc::new(CallLimiter::with_clock(
        Duration::from_secs(3600),
        move || instant,
    ));
    let (base, _) = start(static_handler(vec![(
        "/ws/2/release/",
        StatusCode::OK,
        RELEASE_LOOKUP,
    )]))
    .await;

    client(&base, Arc::clone(&limiter))
        .release(&release_id())
        .await
        .unwrap();

    assert!(
        limiter.reserve_at(instant) >= Duration::from_secs(3599),
        "the release lookup paced through the shared document limiter"
    );
}

const RECORDING_LOOKUP: &str =
    include_str!("fixtures/controlled_musicbrainz_recording_lookup.json");

#[tokio::test]
async fn a_recording_lookup_sends_its_own_includes() {
    let (base, records) = start(static_handler(vec![(
        "/ws/2/recording/",
        StatusCode::OK,
        RECORDING_LOOKUP,
    )]))
    .await;
    let recording = client(&base, Arc::new(CallLimiter::new(Duration::ZERO)))
        .recording(&MbId::parse("30000000-0000-0000-0000-000000000001").unwrap())
        .await
        .unwrap()
        .unwrap();

    assert_eq!(recording.title, "A Track");
    assert_eq!(recording.length_ms, Some(210_000));

    let records = records.lock().unwrap();
    assert!(records[0].contains("inc=artist-credits"));
    assert!(
        !records[0].contains("inc=recordings"),
        "a recording lookup must not request release-only subqueries"
    );
}
