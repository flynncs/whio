use super::*;
use crate::{features::metadata::refresh::DocumentFieldError, platform::pacing::CallLimiter};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{Request, StatusCode},
    response::Response,
    routing::any,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::TcpListener;
use url::Url;

const MBID: &str = "cc197bad-dc9c-440d-a5b5-d52ba2e14234";
const USER_AGENT: &str = "whio/1.0 ( https://example.test/contact )";
const TOKEN: &str = "sentinel-listenbrainz-token-value";
const ALGORITHM: &str =
    "session_based_days_7500_session_300_contribution_5_threshold_10_limit_100_filter_True_skip_30";
const SIMILAR: &str = include_str!("fixtures/controlled_listenbrainz_similar_artists.json");
const TRACKS: &str = include_str!("fixtures/controlled_listenbrainz_top_recordings.json");
const POPULARITY: &str = include_str!("fixtures/controlled_listenbrainz_artist_popularity.json");

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: String,
    chunks: Option<Vec<String>>,
    retry_after: Option<String>,
}

#[derive(Clone)]
struct Call {
    method: String,
    uri: String,
    user_agent: Option<String>,
    authorization: Option<String>,
    body: String,
}

#[derive(Clone)]
struct Stub {
    calls: Arc<Mutex<Vec<Call>>>,
    replies: Arc<Mutex<VecDeque<(String, Reply)>>>,
}

fn reply(status: StatusCode, body: impl Into<String>) -> Reply {
    Reply {
        status,
        body: body.into(),
        chunks: None,
        retry_after: None,
    }
}

async fn start_stub(overrides: Vec<(String, Reply)>) -> (Url, Stub) {
    let state = Stub {
        calls: Arc::new(Mutex::new(Vec::new())),
        replies: Arc::new(Mutex::new(overrides.into())),
    };
    let app = Router::new()
        .fallback(any(respond))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (Url::parse(&format!("http://{address}/")).unwrap(), state)
}

async fn respond(State(state): State<Stub>, request: Request<Body>) -> Response<Body> {
    let (parts, body) = request.into_parts();
    let method = parts.method.to_string();
    let uri = parts.uri.to_string();
    let user_agent = parts
        .headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let authorization = parts
        .headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    drop(parts);
    let body = to_bytes(body, usize::MAX).await.unwrap();
    state.calls.lock().unwrap().push(Call {
        method,
        uri: uri.clone(),
        user_agent,
        authorization,
        body: String::from_utf8(body.to_vec()).unwrap(),
    });
    let selected = {
        let mut replies = state.replies.lock().unwrap();
        replies
            .iter()
            .position(|(pattern, _)| uri.contains(pattern))
            .and_then(|index| replies.remove(index))
            .map(|(_, reply)| reply)
    }
    .unwrap_or_else(|| {
        let body = if uri.starts_with("/similar-artists/json") {
            SIMILAR
        } else if uri.starts_with("/1/popularity/top-recordings-for-artist/") {
            TRACKS
        } else if uri.starts_with("/1/popularity/artist") {
            POPULARITY
        } else {
            "{}"
        };
        reply(StatusCode::OK, body)
    });
    let mut builder = Response::builder().status(selected.status);
    if let Some(retry_after) = selected.retry_after {
        builder = builder.header("retry-after", retry_after);
    }
    if let Some(chunks) = selected.chunks {
        builder
            .header("content-type", "application/json")
            .body(Body::from_stream(tokio_stream::iter(
                chunks.into_iter().map(Ok::<_, std::io::Error>),
            )))
            .unwrap()
    } else {
        builder.body(Body::from(selected.body)).unwrap()
    }
}

fn client(api: &Url, labs: &Url, token: &str, algorithm: &str) -> ListenBrainzClient {
    ListenBrainzClient::with_hosts(
        api.clone(),
        labs.clone(),
        token.to_owned(),
        USER_AGENT.to_owned(),
        algorithm.to_owned(),
        Arc::new(CallLimiter::new(Duration::ZERO)),
    )
    .unwrap()
}

fn source() -> crate::features::music_library::domain::SourceIdentity {
    crate::features::music_library::domain::SourceIdentity::new(
        crate::features::music_library::domain::ProviderId::new("listenbrainz".to_owned()).unwrap(),
        crate::features::music_library::domain::SourceScope::Global,
        MBID.to_owned(),
    )
    .unwrap()
}

fn documents(api: &Url, labs: &Url, algorithm: &str) -> ListenBrainzDocuments {
    ListenBrainzDocuments::with_client(client(api, labs, TOKEN, algorithm))
}

fn field(document: &DocumentOutcome, field: FieldId) -> &DocumentCandidate {
    document
        .candidates
        .iter()
        .find(|candidate| candidate.field == field)
        .unwrap()
}

fn field_error(document: &DocumentOutcome, field: FieldId) -> &DocumentFieldError {
    document
        .errors
        .iter()
        .find(|error| error.field == field)
        .unwrap()
}

fn candidates(document: &DocumentOutcome) -> Vec<FieldId> {
    document
        .candidates
        .iter()
        .map(|candidate| candidate.field)
        .collect()
}

#[tokio::test]
async fn the_similar_artists_call_targets_the_labs_host_with_the_algorithm() {
    let (labs, stub) = start_stub(Vec::new()).await;
    let client = client(&labs, &labs, "", ALGORITHM);
    let payload = client.fetch_similar_artists(MBID).await.unwrap();
    assert_eq!(payload.as_array().unwrap().len(), 3);
    let calls = stub.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "GET");
    assert_eq!(
        calls[0].uri,
        format!("/similar-artists/json?artist_mbids={MBID}&algorithm={ALGORITHM}")
    );
    assert_eq!(calls[0].user_agent.as_deref(), Some(USER_AGENT));
    assert_eq!(calls[0].authorization, None);
}

#[tokio::test]
async fn the_top_recordings_call_streams_the_api_host_with_the_token() {
    let (api, stub) = start_stub(Vec::new()).await;
    let entries: Value = serde_json::from_str(TRACKS).unwrap();
    let payload = client(&api, &api, TOKEN, ALGORITHM)
        .fetch_top_recordings(MBID)
        .await
        .unwrap();
    assert_eq!(payload, entries.as_array().unwrap().clone());
    let calls = stub.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "GET");
    assert_eq!(
        calls[0].uri,
        format!("/1/popularity/top-recordings-for-artist/{MBID}")
    );
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some(format!("Token {TOKEN}").as_str())
    );
}

#[tokio::test]
async fn the_artist_popularity_call_posts_the_single_mbid() {
    let (api, stub) = start_stub(Vec::new()).await;
    let expected: Value = serde_json::from_str(POPULARITY).unwrap();
    let payload = client(&api, &api, TOKEN, ALGORITHM)
        .fetch_artist_popularity(MBID)
        .await
        .unwrap();
    assert_eq!(payload, expected);
    let calls = stub.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].method, "POST");
    assert_eq!(calls[0].uri, "/1/popularity/artist");
    assert_eq!(
        serde_json::from_str::<Value>(&calls[0].body).unwrap(),
        json!({"artist_mbids":[MBID]})
    );
    assert_eq!(
        calls[0].authorization.as_deref(),
        Some(format!("Token {TOKEN}").as_str())
    );
}

#[tokio::test]
async fn the_token_is_not_sent_to_the_labs_host() {
    let (labs, stub) = start_stub(Vec::new()).await;
    client(&labs, &labs, TOKEN, ALGORITHM)
        .fetch_similar_artists(MBID)
        .await
        .unwrap();
    assert_eq!(stub.calls.lock().unwrap()[0].authorization, None);
}

#[tokio::test]
async fn a_streamed_list_stops_at_the_instance_limit() {
    let mut entries: Value = serde_json::from_str(TRACKS).unwrap();
    let list = entries.as_array_mut().unwrap();
    while list.len() < 51 {
        list.push(
            json!({"recording_mbid":format!("recording-{}", list.len()), "recording_name":"Track"}),
        );
    }
    let mut response = reply(StatusCode::OK, entries.to_string());
    response.chunks = Some(vec![
        entries.to_string(),
        "TRAILING-BODY-NOT-JSON".to_owned(),
    ]);
    let (api, _) = start_stub(vec![("top-recordings-for-artist".to_owned(), response)]).await;
    let payload = client(&api, &api, "", ALGORITHM)
        .fetch_top_recordings(MBID)
        .await
        .unwrap();
    assert_eq!(payload.len(), 50);
}

#[tokio::test]
async fn a_chunk_split_across_an_entry_is_still_parsed() {
    let entries: Value = serde_json::from_str(TRACKS).unwrap();
    let serialized = entries.to_string();
    let chunks = serialized
        .as_bytes()
        .chunks(7)
        .map(|bytes| String::from_utf8(bytes.to_vec()).unwrap())
        .collect();
    let mut response = reply(StatusCode::OK, "");
    response.chunks = Some(chunks);
    let (api, _) = start_stub(vec![("top-recordings-for-artist".to_owned(), response)]).await;
    assert_eq!(
        client(&api, &api, "", ALGORITHM)
            .fetch_top_recordings(MBID)
            .await
            .unwrap(),
        entries.as_array().unwrap().clone()
    );
}

#[tokio::test]
async fn a_streamed_body_that_is_not_a_list_is_a_resolution_failure() {
    let mut response = reply(StatusCode::OK, "");
    response.chunks = Some(vec!["{\"recording_mbid\":\"x\"}".to_owned()]);
    let (api, _) = start_stub(vec![("top-recordings-for-artist".to_owned(), response)]).await;
    assert_eq!(
        client(&api, &api, "", ALGORITHM)
            .fetch_top_recordings(MBID)
            .await
            .unwrap_err(),
        DocumentError::ResolutionFailed
    );
}

#[tokio::test]
async fn a_truncated_streamed_body_is_a_resolution_failure() {
    let mut response = reply(StatusCode::OK, "");
    response.chunks = Some(vec!["[{\"recording_mbid\":\"x\"}".to_owned()]);
    let (api, _) = start_stub(vec![("top-recordings-for-artist".to_owned(), response)]).await;
    assert_eq!(
        client(&api, &api, "", ALGORITHM)
            .fetch_top_recordings(MBID)
            .await
            .unwrap_err(),
        DocumentError::ResolutionFailed
    );
}

#[tokio::test]
async fn a_429_propagates_the_retry_after_delay_as_unavailable() {
    let mut response = reply(StatusCode::TOO_MANY_REQUESTS, "{}");
    response.retry_after = Some("7".to_owned());
    let (api, _) = start_stub(vec![("similar-artists/json".to_owned(), response)]).await;
    assert_eq!(
        client(&api, &api, "", ALGORITHM)
            .fetch_similar_artists(MBID)
            .await
            .unwrap_err(),
        DocumentError::Unavailable {
            retry_after: Some(Duration::from_secs(7))
        }
    );
}

#[tokio::test]
async fn server_failures_are_unavailable_and_retryable() {
    let (api, _) = start_stub(vec![(
        "similar-artists/json".to_owned(),
        reply(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    )])
    .await;
    assert_eq!(
        client(&api, &api, "", ALGORITHM)
            .fetch_similar_artists(MBID)
            .await
            .unwrap_err(),
        DocumentError::Unavailable { retry_after: None }
    );
}

#[tokio::test]
async fn a_404_is_a_typed_source_not_found() {
    let (api, _) = start_stub(vec![(
        "similar-artists/json".to_owned(),
        reply(StatusCode::NOT_FOUND, "not found"),
    )])
    .await;
    assert_eq!(
        client(&api, &api, "", ALGORITHM)
            .fetch_similar_artists(MBID)
            .await
            .unwrap_err(),
        DocumentError::NotFound
    );
}

#[tokio::test]
async fn a_rejected_request_is_a_resolution_failure() {
    let (api, _) = start_stub(vec![(
        "similar-artists/json".to_owned(),
        reply(StatusCode::BAD_REQUEST, "bad request"),
    )])
    .await;
    assert_eq!(
        client(&api, &api, "", ALGORITHM)
            .fetch_similar_artists(MBID)
            .await
            .unwrap_err(),
        DocumentError::ResolutionFailed
    );
}

#[tokio::test]
async fn timeout_and_connection_failures_are_unavailable_and_retryable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let closed = Url::parse(&format!("http://{address}/")).unwrap();
    assert_eq!(
        client(&closed, &closed, TOKEN, ALGORITHM)
            .fetch_similar_artists(MBID)
            .await
            .unwrap_err(),
        DocumentError::Unavailable { retry_after: None }
    );
}

#[tokio::test]
async fn a_non_json_body_is_a_resolution_failure() {
    let (api, _) = start_stub(vec![(
        "similar-artists/json".to_owned(),
        reply(StatusCode::OK, "<html>not json</html>"),
    )])
    .await;
    assert_eq!(
        client(&api, &api, "", ALGORITHM)
            .fetch_similar_artists(MBID)
            .await
            .unwrap_err(),
        DocumentError::ResolutionFailed
    );
}

#[test]
fn consecutive_calls_are_paced_at_one_per_second() {
    let limiter = CallLimiter::new(Duration::from_secs(1));
    let now = std::time::Instant::now();
    assert_eq!(limiter.reserve_at(now), Duration::ZERO);
    assert_eq!(limiter.reserve_at(now), Duration::from_secs(1));
}

#[tokio::test]
async fn the_token_is_not_logged_or_echoed_in_an_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let closed = Url::parse(&format!("http://{address}/")).unwrap();
    let error = client(&closed, &closed, TOKEN, ALGORITHM)
        .fetch_similar_artists(MBID)
        .await
        .unwrap_err();
    assert_eq!(error, DocumentError::Unavailable { retry_after: None });
    assert!(!error.to_string().contains(TOKEN));
}

#[test]
fn configured_token_and_algorithm_read_the_metadata_environment() {
    let vars = crate::platform::config::Vars::from([
        (
            "WHIO_LISTENBRAINZ_METADATA_ENABLED".to_owned(),
            "true".to_owned(),
        ),
        (
            "WHIO_LISTENBRAINZ_TOKEN".to_owned(),
            "  resolved-token  ".to_owned(),
        ),
        (
            "WHIO_LISTENBRAINZ_ALGORITHM".to_owned(),
            "  pinned-algorithm-id  ".to_owned(),
        ),
    ]);
    let config = super::super::documents_config::from_vars(&vars)
        .unwrap()
        .unwrap();
    assert_eq!(config.token, "resolved-token");
    assert_eq!(config.algorithm, "pinned-algorithm-id");
    let vars = crate::platform::config::Vars::from([
        (
            "WHIO_LISTENBRAINZ_METADATA_ENABLED".to_owned(),
            "true".to_owned(),
        ),
        ("WHIO_LISTENBRAINZ_TOKEN".to_owned(), "token".to_owned()),
        ("WHIO_LISTENBRAINZ_ALGORITHM".to_owned(), "  ".to_owned()),
    ]);
    assert_eq!(
        super::super::documents_config::from_vars(&vars)
            .unwrap()
            .unwrap()
            .algorithm,
        super::super::documents_config::DEFAULT_ALGORITHM
    );
}

#[test]
fn the_default_algorithm_is_the_pinned_labs_string() {
    assert_eq!(super::super::documents_config::DEFAULT_ALGORITHM, ALGORITHM);
}

#[tokio::test]
async fn one_document_yields_all_four_fields_with_one_call_each() {
    let (host, stub) = start_stub(Vec::new()).await;
    let docs = documents(&host, &host, ALGORITHM);
    let result = docs
        .fetch_document(
            &source(),
            &[
                FieldId::Similar,
                FieldId::TopTracks,
                FieldId::MetricListen,
                FieldId::MetricListener,
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        candidates(&result),
        [
            FieldId::Similar,
            FieldId::TopTracks,
            FieldId::MetricListen,
            FieldId::MetricListener
        ]
    );
    assert!(result.errors.is_empty());
    assert_eq!(result.address, source());
    assert_eq!(stub.calls.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn the_similar_ranking_scope_is_the_pinned_algorithm_verbatim() {
    let (host, _) = start_stub(Vec::new()).await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::Similar])
        .await
        .unwrap();
    let FieldValue::Candidates(items) = &field(&result, FieldId::Similar).value else {
        panic!("similar results are candidates")
    };
    assert!(
        items
            .iter()
            .all(|item| item.ranking_scope.as_deref() == Some(ALGORITHM))
    );
}

#[tokio::test]
async fn the_top_tracks_ranking_scope_names_the_endpoint() {
    let (host, _) = start_stub(Vec::new()).await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::TopTracks])
        .await
        .unwrap();
    let FieldValue::Candidates(items) = &field(&result, FieldId::TopTracks).value else {
        panic!("top tracks are candidates")
    };
    assert!(items.iter().all(|item| item.ranking_scope.as_deref()
        == Some("listenbrainz.popularity.top-recordings-for-artist")));
}

#[tokio::test]
async fn similar_candidates_keep_the_provider_order() {
    let entries: Value = serde_json::from_str(SIMILAR).unwrap();
    let (host, _) = start_stub(Vec::new()).await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::Similar])
        .await
        .unwrap();
    let FieldValue::Candidates(items) = &field(&result, FieldId::Similar).value else {
        panic!("similar results are candidates")
    };
    assert_eq!(
        items
            .iter()
            .map(|item| item.name.as_str())
            .collect::<Vec<_>>(),
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["name"].as_str().unwrap())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        items.iter().map(|item| item.rank).collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2)]
    );
    assert_eq!(
        items.iter().map(|item| item.score).collect::<Vec<_>>(),
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["score"].as_f64())
            .collect::<Vec<_>>()
    );
    assert_eq!(
        items
            .iter()
            .map(|item| item.external_id.as_deref().unwrap())
            .collect::<Vec<_>>(),
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["artist_mbid"].as_str().unwrap())
            .collect::<Vec<_>>()
    );
    assert!(items.iter().all(|item| item.external_id.is_some()));
}

#[tokio::test]
async fn the_artist_metric_never_comes_from_a_recording_count() {
    let popularity: Value = serde_json::from_str(POPULARITY).unwrap();
    let (host, _) = start_stub(Vec::new()).await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(
            &source(),
            &[
                FieldId::MetricListen,
                FieldId::MetricListener,
                FieldId::TopTracks,
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        field(&result, FieldId::MetricListen).value,
        FieldValue::Count(popularity[0]["total_listen_count"].as_u64().unwrap())
    );
    assert_eq!(
        field(&result, FieldId::MetricListener).value,
        FieldValue::Count(popularity[0]["total_user_count"].as_u64().unwrap())
    );
    let FieldValue::Candidates(tracks) = &field(&result, FieldId::TopTracks).value else {
        panic!("tracks are candidates")
    };
    assert!(
        tracks.iter().all(|track| track.rank
            != Some(popularity[0]["total_listen_count"].as_u64().unwrap() as u32))
    );
}

#[tokio::test]
async fn metric_population_is_present_and_period_is_absent() {
    let (host, _) = start_stub(Vec::new()).await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::MetricListen, FieldId::MetricListener])
        .await
        .unwrap();
    for (field_id, unit) in [
        (FieldId::MetricListen, "listens"),
        (FieldId::MetricListener, "listeners"),
    ] {
        let metric = field(&result, field_id).metric.as_ref().unwrap();
        assert_eq!(metric.unit, unit);
        assert_eq!(metric.population.as_deref(), Some("ListenBrainz users"));
        assert_eq!(metric.period, None);
    }
}

#[tokio::test]
async fn every_candidate_licence_forbids_redistribution() {
    let (host, _) = start_stub(Vec::new()).await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(
            &source(),
            &[
                FieldId::Similar,
                FieldId::TopTracks,
                FieldId::MetricListen,
                FieldId::MetricListener,
            ],
        )
        .await
        .unwrap();
    for candidate in &result.candidates {
        assert_eq!(candidate.licence.id, "listenbrainz");
        assert!(!candidate.licence.redistribution_allowed);
        assert!(candidate.licence.cache_allowed);
        assert!(candidate.licence.attribution_required);
        assert!(!candidate.licence.share_alike);
        assert!(!candidate.licence.non_commercial);
    }
}

#[tokio::test]
async fn recordings_carry_their_own_listen_and_user_counts() {
    let entries: Value = serde_json::from_str(TRACKS).unwrap();
    let (host, _) = start_stub(Vec::new()).await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::TopTracks])
        .await
        .unwrap();
    let FieldValue::Candidates(items) = &field(&result, FieldId::TopTracks).value else {
        panic!("tracks are candidates")
    };
    assert_eq!(
        items.iter().map(|item| item.rank).collect::<Vec<_>>(),
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["total_listen_count"]
                .as_u64()
                .map(|count| count as u32))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        items.iter().map(|item| item.score).collect::<Vec<_>>(),
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["total_user_count"].as_u64().map(|count| count as f64))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        items
            .iter()
            .map(|item| item.external_id.as_deref().unwrap())
            .collect::<Vec<_>>(),
        entries
            .as_array()
            .unwrap()
            .iter()
            .map(|entry| entry["recording_mbid"].as_str().unwrap())
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn empty_lists_are_absence_without_an_error() {
    let (host, _) = start_stub(vec![
        (
            "similar-artists/json".to_owned(),
            reply(StatusCode::OK, "[]"),
        ),
        (
            "top-recordings-for-artist".to_owned(),
            reply(StatusCode::OK, "[]"),
        ),
        (
            "/1/popularity/artist".to_owned(),
            reply(StatusCode::OK, "[]"),
        ),
    ])
    .await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(
            &source(),
            &[
                FieldId::Similar,
                FieldId::TopTracks,
                FieldId::MetricListen,
                FieldId::MetricListener,
            ],
        )
        .await
        .unwrap();
    assert!(result.candidates.is_empty());
    assert!(result.errors.is_empty());
}

#[tokio::test]
async fn null_metric_counts_are_absence_without_an_error() {
    let (host, _) = start_stub(vec![(
        "/1/popularity/artist".to_owned(),
        reply(
            StatusCode::OK,
            include_str!("fixtures/controlled_listenbrainz_artist_popularity_null_counts.json"),
        ),
    )])
    .await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::MetricListen, FieldId::MetricListener])
        .await
        .unwrap();
    assert!(result.candidates.is_empty());
    assert!(result.errors.is_empty());
}

#[tokio::test]
async fn a_recording_without_a_title_is_omitted_not_invented() {
    let (host, _) = start_stub(vec![(
        "top-recordings-for-artist".to_owned(),
        reply(
            StatusCode::OK,
            include_str!("fixtures/controlled_listenbrainz_top_recordings_without_title.json"),
        ),
    )])
    .await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::TopTracks])
        .await
        .unwrap();
    assert!(result.candidates.is_empty());
    assert!(result.errors.is_empty());
}

#[tokio::test]
async fn the_lists_are_truncated_to_the_instance_limit() {
    let (host, _) = start_stub(vec![
        (
            "similar-artists/json".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_listenbrainz_similar_artists_over_limit.json"),
            ),
        ),
        (
            "top-recordings-for-artist".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_listenbrainz_top_recordings_over_limit.json"),
            ),
        ),
    ])
    .await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::Similar, FieldId::TopTracks])
        .await
        .unwrap();
    for field_id in [FieldId::Similar, FieldId::TopTracks] {
        let FieldValue::Candidates(items) = &field(&result, field_id).value else {
            panic!("list fields are candidates")
        };
        assert_eq!(items.len(), 50);
    }
}

#[tokio::test]
async fn a_failing_similar_call_is_a_field_error_that_spares_the_others() {
    let (host, _) = start_stub(vec![(
        "similar-artists/json".to_owned(),
        reply(StatusCode::SERVICE_UNAVAILABLE, "{}"),
    )])
    .await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(
            &source(),
            &[
                FieldId::Similar,
                FieldId::TopTracks,
                FieldId::MetricListen,
                FieldId::MetricListener,
            ],
        )
        .await
        .unwrap();
    assert_eq!(
        candidates(&result),
        [
            FieldId::TopTracks,
            FieldId::MetricListen,
            FieldId::MetricListener
        ]
    );
    assert_eq!(
        field_error(&result, FieldId::Similar).code,
        "provider_unavailable"
    );
    assert!(field_error(&result, FieldId::Similar).retryable);
}

#[tokio::test]
async fn a_failing_popularity_call_errors_both_metric_fields() {
    let (host, _) = start_stub(vec![(
        "/1/popularity/artist".to_owned(),
        reply(StatusCode::BAD_REQUEST, "{}"),
    )])
    .await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(
            &source(),
            &[
                FieldId::Similar,
                FieldId::TopTracks,
                FieldId::MetricListen,
                FieldId::MetricListener,
            ],
        )
        .await
        .unwrap();
    assert_eq!(candidates(&result), [FieldId::Similar, FieldId::TopTracks]);
    assert_eq!(
        result
            .errors
            .iter()
            .map(|error| error.field)
            .collect::<Vec<_>>(),
        [FieldId::MetricListen, FieldId::MetricListener]
    );
    assert_eq!(
        field_error(&result, FieldId::MetricListen).code,
        "resolution_failed"
    );
}

#[tokio::test]
async fn only_the_requested_fields_are_fetched() {
    let (host, stub) = start_stub(Vec::new()).await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::Similar])
        .await
        .unwrap();
    assert_eq!(candidates(&result), [FieldId::Similar]);
    assert_eq!(stub.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn the_requested_fields_filter_candidates_and_errors() {
    let (host, _) = start_stub(vec![(
        "/1/popularity/artist".to_owned(),
        reply(StatusCode::SERVICE_UNAVAILABLE, "{}"),
    )])
    .await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::MetricListen])
        .await
        .unwrap();
    assert!(result.candidates.is_empty());
    assert_eq!(
        result
            .errors
            .iter()
            .map(|error| error.field)
            .collect::<Vec<_>>(),
        [FieldId::MetricListen]
    );
}

#[tokio::test]
async fn a_wrong_typed_body_is_a_resolution_failure() {
    let wrong_recording = serde_json::from_str::<Value>(include_str!(
        "fixtures/controlled_listenbrainz_top_recordings_without_title.json"
    ))
    .unwrap()[0]
        .to_string();
    let cases = [
        (
            "similar-artists/json",
            include_str!("fixtures/controlled_listenbrainz_similar_artists_wrong_type.json")
                .to_owned(),
        ),
        (
            "similar-artists/json",
            include_str!("fixtures/controlled_listenbrainz_similar_artists_wrong_entry_type.json")
                .to_owned(),
        ),
        ("top-recordings-for-artist", wrong_recording),
        (
            "/1/popularity/artist",
            include_str!("fixtures/controlled_listenbrainz_artist_popularity_wrong_type.json")
                .to_owned(),
        ),
        (
            "/1/popularity/artist",
            include_str!(
                "fixtures/controlled_listenbrainz_artist_popularity_wrong_entry_type.json"
            )
            .to_owned(),
        ),
    ];
    for (pattern, body) in cases {
        let (host, _) = start_stub(vec![(pattern.to_owned(), reply(StatusCode::OK, body))]).await;
        let field_id = if pattern.contains("similar") {
            FieldId::Similar
        } else if pattern.contains("top-recordings") {
            FieldId::TopTracks
        } else {
            FieldId::MetricListen
        };
        let result = documents(&host, &host, ALGORITHM)
            .fetch_document(&source(), &[field_id])
            .await
            .unwrap();
        assert!(result.candidates.is_empty());
        assert_eq!(field_error(&result, field_id).code, "resolution_failed");
    }
}

#[tokio::test]
async fn a_wrong_similar_reference_is_a_resolution_failure() {
    let (host, _) = start_stub(vec![(
        "similar-artists/json".to_owned(),
        reply(
            StatusCode::OK,
            include_str!("fixtures/controlled_listenbrainz_similar_artists_wrong_reference.json"),
        ),
    )])
    .await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::Similar])
        .await
        .unwrap();
    assert_eq!(
        field_error(&result, FieldId::Similar).code,
        "resolution_failed"
    );
}

#[tokio::test]
async fn a_wrong_popularity_identity_is_a_resolution_failure() {
    let (host, _) = start_stub(vec![(
        "/1/popularity/artist".to_owned(),
        reply(
            StatusCode::OK,
            include_str!("fixtures/controlled_listenbrainz_artist_popularity_wrong_identity.json"),
        ),
    )])
    .await;
    let result = documents(&host, &host, ALGORITHM)
        .fetch_document(&source(), &[FieldId::MetricListen, FieldId::MetricListener])
        .await
        .unwrap();
    assert_eq!(
        field_error(&result, FieldId::MetricListen).code,
        "resolution_failed"
    );
    assert_eq!(
        field_error(&result, FieldId::MetricListener).code,
        "resolution_failed"
    );
}

#[test]
fn no_controlled_fixture_carries_a_credential() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/integrations/listenbrainz/fixtures");
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("controlled_listenbrainz_")
        {
            continue;
        }
        let body = std::fs::read_to_string(path).unwrap();
        assert!(!body.contains(TOKEN));
        assert!(!body.contains("Authorization"));
        assert!(!body.to_lowercase().contains("token"));
    }
}
