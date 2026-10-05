use super::*;
use axum::{
    Router,
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::Response,
    routing::any,
};
use serde_json::json;
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::net::TcpListener;
use url::Url;

use crate::platform::pacing::CallLimiter;

const ARTIST: &str = include_str!("fixtures/controlled_musicbrainz_artist_document.json");
const WIKIDATA: &str = include_str!("fixtures/controlled_wikidata_entity.json");
const COMMONS: &str = include_str!("fixtures/controlled_commons_imageinfo.json");
const SUMMARY: &str = include_str!("fixtures/controlled_wikipedia_summary.json");
const EXTRACT: &str = include_str!("fixtures/controlled_wikipedia_extract.json");
const MBID: &str = "cc197bad-dc9c-440d-a5b5-d52ba2e14234";

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: String,
    retry_after: Option<String>,
}

type Call = (String, Option<String>);

#[derive(Clone)]
struct Stub {
    calls: Arc<Mutex<Vec<Call>>>,
    replies: Arc<Mutex<VecDeque<(String, Reply)>>>,
}

fn reply(status: StatusCode, body: impl Into<String>) -> Reply {
    Reply {
        status,
        body: body.into(),
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
    let uri = request.uri().to_string();
    let user_agent = request
        .headers()
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    state.calls.lock().unwrap().push((uri.clone(), user_agent));
    let override_reply = {
        let mut replies = state.replies.lock().unwrap();
        replies
            .iter()
            .position(|(pattern, _)| uri.contains(pattern))
            .and_then(|index| replies.remove(index))
            .map(|(_, reply)| reply)
    };
    let selected = override_reply.unwrap_or_else(|| {
        let body = if uri.starts_with("/ws/2/artist/") {
            ARTIST
        } else if uri.contains("action=wbgetentities") {
            WIKIDATA
        } else if uri.contains("prop=imageinfo") {
            COMMONS
        } else if uri.starts_with("/api/rest_v1/page/summary/") {
            SUMMARY
        } else if uri.contains("prop=extracts") {
            EXTRACT
        } else {
            "{}"
        };
        reply(StatusCode::OK, body)
    });
    let mut builder = Response::builder().status(selected.status);
    if let Some(value) = selected.retry_after {
        builder = builder.header("retry-after", value);
    }
    builder.body(Body::from(selected.body)).unwrap()
}

fn client(base: &Url) -> MusicBrainzClient {
    client_with_limiter(base, Arc::new(CallLimiter::new(Duration::ZERO)))
}

fn client_with_limiter(base: &Url, limiter: Arc<CallLimiter>) -> MusicBrainzClient {
    MusicBrainzClient::with_hosts(
        base.clone(),
        base.clone(),
        base.clone(),
        super::super::client::WikipediaHosts::Fixed(base.clone()),
        "musicbrainz-agent".to_owned(),
        "wikimedia-agent".to_owned(),
        limiter,
    )
    .unwrap()
}

fn source() -> SourceIdentity {
    SourceIdentity::new(
        crate::features::music_library::domain::ProviderId::new("musicbrainz".to_owned()).unwrap(),
        crate::features::music_library::domain::SourceScope::Global,
        MBID.to_owned(),
    )
    .unwrap()
}

fn fields() -> [FieldId; 7] {
    [
        FieldId::IdentityMbid,
        FieldId::PageUrl,
        FieldId::Tag,
        FieldId::BiographySummary,
        FieldId::BiographyContent,
        FieldId::BiographyLanguage,
        FieldId::ImagePortrait,
    ]
}

fn find(document: &DocumentOutcome, field: FieldId) -> Vec<&DocumentCandidate> {
    document
        .candidates
        .iter()
        .filter(|candidate| candidate.field == field)
        .collect()
}

fn errors(document: &DocumentOutcome, field: FieldId) -> Vec<&DocumentFieldError> {
    document
        .errors
        .iter()
        .filter(|error| error.field == field)
        .collect()
}

#[tokio::test]
async fn the_artist_document_is_requested_once_with_the_user_agent() {
    let (base, stub) = start_stub(Vec::new()).await;
    let client = client(&base);
    let payload = client.fetch_artist(MBID).await.unwrap();
    assert_eq!(payload["id"], MBID);
    let calls = stub.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].0,
        format!("/ws/2/artist/{MBID}?inc=url-rels%2Btags%2Bgenres&fmt=json")
    );
    assert_eq!(calls[0].1.as_deref(), Some("musicbrainz-agent"));
}

#[tokio::test]
async fn the_wikidata_entity_call_carries_the_wikimedia_user_agent() {
    let (base, stub) = start_stub(Vec::new()).await;
    let payload = client(&base).fetch_wikidata_entity("Q11647").await.unwrap();
    assert!(payload["entities"]["Q11647"].is_object());
    let calls = stub.calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert!(calls[0].0.starts_with("/w/api.php?"));
    assert!(calls[0].0.contains("action=wbgetentities"));
    assert!(calls[0].0.contains("ids=Q11647"));
    assert!(calls[0].0.contains("props=claims%7Csitelinks"));
    assert!(calls[0].0.contains("format=json"));
    assert!(calls[0].0.contains("maxlag=5"));
    assert_eq!(calls[0].1.as_deref(), Some("wikimedia-agent"));
}

#[tokio::test]
async fn the_commons_imageinfo_call_reads_extmetadata_and_url() {
    let (base, stub) = start_stub(Vec::new()).await;
    let payload = client(&base)
        .fetch_commons_imageinfo("ColdplayWembley.jpg")
        .await
        .unwrap();
    assert!(payload["query"]["pages"].is_object());
    let calls = stub.calls.lock().unwrap();
    assert!(calls[0].0.contains("prop=imageinfo"));
    assert!(calls[0].0.contains("iiprop=extmetadata%7Curl"));
    assert!(calls[0].0.contains("titles=File%3AColdplayWembley.jpg"));
    assert!(calls[0].0.contains("action=query"));
    assert!(calls[0].0.contains("format=json"));
    assert!(calls[0].0.contains("maxlag=5"));
    assert_eq!(calls[0].1.as_deref(), Some("wikimedia-agent"));
}

#[tokio::test]
async fn the_wikipedia_summary_call_uses_the_page_title_path() {
    let (base, stub) = start_stub(Vec::new()).await;
    client(&base)
        .fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")
        .await
        .unwrap();
    assert_eq!(
        stub.calls.lock().unwrap()[0].0,
        "/api/rest_v1/page/summary/Coldplay"
    );
}

#[tokio::test]
async fn the_wikipedia_extract_call_uses_the_action_api() {
    let (base, stub) = start_stub(Vec::new()).await;
    client(&base)
        .fetch_wikipedia_extract("de.wikipedia.org", "Coldplay")
        .await
        .unwrap();
    let calls = stub.calls.lock().unwrap();
    assert!(calls[0].0.starts_with("/w/api.php?"));
    assert!(calls[0].0.contains("action=query"));
    assert!(calls[0].0.contains("prop=extracts"));
    assert!(calls[0].0.contains("titles=Coldplay"));
    assert!(calls[0].0.contains("explaintext=1"));
    assert!(calls[0].0.contains("format=json"));
    assert!(calls[0].0.contains("maxlag=5"));
    assert_eq!(calls[0].1.as_deref(), Some("wikimedia-agent"));
}

#[tokio::test]
async fn the_wikimedia_user_agent_falls_back_to_the_musicbrainz_one() {
    let vars = crate::platform::config::Vars::from([(
        "WHIO_MUSICBRAINZ_USER_AGENT".to_owned(),
        "shared-agent".to_owned(),
    )]);
    let config = super::super::config::from_vars(&vars).unwrap().unwrap();
    assert_eq!(config.wikimedia_user_agent, "shared-agent");
    let (base, stub) = start_stub(Vec::new()).await;
    let client = MusicBrainzClient::with_hosts(
        base.clone(),
        base.clone(),
        base.clone(),
        super::super::client::WikipediaHosts::Fixed(base.clone()),
        config.user_agent,
        config.wikimedia_user_agent,
        Arc::new(CallLimiter::new(Duration::ZERO)),
    )
    .unwrap();
    client
        .fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")
        .await
        .unwrap();
    assert_eq!(
        stub.calls.lock().unwrap()[0].1.as_deref(),
        Some("shared-agent")
    );
}

#[tokio::test]
async fn an_unknown_mbid_is_a_typed_not_found() {
    let (base, _) = start_stub(vec![(
        "/ws/2/artist/".to_owned(),
        reply(StatusCode::NOT_FOUND, "{}"),
    )])
    .await;
    assert_eq!(
        client(&base).fetch_artist(MBID).await.unwrap_err(),
        DocumentError::NotFound
    );
}

#[tokio::test]
async fn a_musicbrainz_429_is_unavailable_without_a_documented_delay() {
    let (base, _) = start_stub(vec![(
        "/ws/2/artist/".to_owned(),
        reply(StatusCode::TOO_MANY_REQUESTS, "{}"),
    )])
    .await;
    assert_eq!(
        client(&base).fetch_artist(MBID).await.unwrap_err(),
        DocumentError::Unavailable { retry_after: None }
    );
}

#[tokio::test]
async fn a_wikimedia_429_propagates_the_retry_after_delay() {
    let mut response = reply(StatusCode::TOO_MANY_REQUESTS, "{}");
    response.retry_after = Some("9".to_owned());
    let (base, _) = start_stub(vec![("/api/rest_v1".to_owned(), response)]).await;
    assert_eq!(
        client(&base)
            .fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")
            .await
            .unwrap_err(),
        DocumentError::Unavailable {
            retry_after: Some(Duration::from_secs(9))
        }
    );
}

#[tokio::test]
async fn a_wikimedia_503_propagates_the_retry_after_delay() {
    let mut response = reply(StatusCode::SERVICE_UNAVAILABLE, "unavailable");
    response.retry_after = Some("3".to_owned());
    let (base, _) = start_stub(vec![("/w/api.php".to_owned(), response)]).await;
    assert_eq!(
        client(&base)
            .fetch_wikipedia_extract("en.wikipedia.org", "Coldplay")
            .await
            .unwrap_err(),
        DocumentError::Unavailable {
            retry_after: Some(Duration::from_secs(3))
        }
    );
}

#[tokio::test]
async fn server_failures_are_unavailable_and_retryable() {
    let (base, _) = start_stub(vec![(
        "/ws/2/artist/".to_owned(),
        reply(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    )])
    .await;
    assert_eq!(
        client(&base).fetch_artist(MBID).await.unwrap_err(),
        DocumentError::Unavailable { retry_after: None }
    );
}

#[tokio::test]
async fn timeout_and_connection_failures_are_unavailable_and_retryable() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    let closed = Url::parse(&format!("http://{address}/")).unwrap();
    assert_eq!(
        client(&closed).fetch_artist(MBID).await.unwrap_err(),
        DocumentError::Unavailable { retry_after: None }
    );
}

#[tokio::test]
async fn a_non_json_body_is_a_resolution_failure() {
    let (base, _) = start_stub(vec![(
        "/ws/2/artist/".to_owned(),
        reply(StatusCode::OK, "<html>not json</html>"),
    )])
    .await;
    assert_eq!(
        client(&base).fetch_artist(MBID).await.unwrap_err(),
        DocumentError::ResolutionFailed
    );
}

#[tokio::test]
async fn a_client_error_is_a_resolution_failure() {
    let (base, _) = start_stub(vec![(
        "/ws/2/artist/".to_owned(),
        reply(StatusCode::BAD_REQUEST, "bad request"),
    )])
    .await;
    assert_eq!(
        client(&base).fetch_artist(MBID).await.unwrap_err(),
        DocumentError::ResolutionFailed
    );
}

#[tokio::test]
async fn the_musicbrainz_host_is_paced_between_requests() {
    let limiter = CallLimiter::new(Duration::from_secs(1));
    let now = std::time::Instant::now();
    assert_eq!(limiter.reserve_at(now), Duration::ZERO);
    assert_eq!(limiter.reserve_at(now), Duration::from_secs(1));
}

#[tokio::test]
async fn a_wikimedia_host_is_not_paced_between_requests() {
    let (base, _) = start_stub(Vec::new()).await;
    let limiter = Arc::new(CallLimiter::new(Duration::from_secs(1)));
    let client = client_with_limiter(&base, Arc::clone(&limiter));
    client
        .fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")
        .await
        .unwrap();
    client
        .fetch_wikipedia_summary("en.wikipedia.org", "Coldplay")
        .await
        .unwrap();
    let now = std::time::Instant::now();
    assert_eq!(limiter.reserve_at(now), Duration::ZERO);
}

#[tokio::test]
async fn the_user_agent_is_not_logged_or_echoed_in_an_error() {
    let (base, _) = start_stub(vec![(
        "/ws/2/artist/".to_owned(),
        reply(StatusCode::SERVICE_UNAVAILABLE, "private transport details"),
    )])
    .await;
    let error = client(&base).fetch_artist(MBID).await.unwrap_err();
    assert_eq!(error, DocumentError::Unavailable { retry_after: None });
    assert!(!error.to_string().contains("musicbrainz-agent"));
    assert!(!error.to_string().contains("private transport details"));
}

#[test]
fn configured_user_agents_read_the_resolver_environment() {
    let vars = crate::platform::config::Vars::from([
        (
            "WHIO_MUSICBRAINZ_USER_AGENT".to_owned(),
            "  whio/1.0 ( a )  ".to_owned(),
        ),
        (
            "WHIO_WIKIMEDIA_USER_AGENT".to_owned(),
            "  whio/1.0 ( b )  ".to_owned(),
        ),
    ]);
    let config = super::super::config::from_vars(&vars).unwrap().unwrap();
    assert_eq!(config.user_agent, "whio/1.0 ( a )");
    assert_eq!(config.wikimedia_user_agent, "whio/1.0 ( b )");
}

#[tokio::test]
async fn one_mbid_document_returns_identity_page_url_tags_and_the_chain() {
    let (base, stub) = start_stub(Vec::new()).await;
    let documents = MusicBrainzDocuments::with_client(Arc::new(client(&base)));
    let document = documents.fetch(&source(), "en", &fields()).await.unwrap();
    assert_eq!(document.address, source());
    assert!(document.errors.is_empty());
    assert_eq!(
        document
            .candidates
            .iter()
            .map(|candidate| candidate.field)
            .collect::<Vec<_>>(),
        [
            FieldId::IdentityMbid,
            FieldId::PageUrl,
            FieldId::PageUrl,
            FieldId::PageUrl,
            FieldId::PageUrl,
            FieldId::PageUrl,
            FieldId::Tag,
            FieldId::ImagePortrait,
            FieldId::BiographySummary,
            FieldId::BiographyContent,
            FieldId::BiographyLanguage,
        ]
    );
    let identity = find(&document, FieldId::IdentityMbid)[0];
    assert_eq!(identity.value, FieldValue::Text(MBID.to_owned()));
    assert_eq!(identity.source.provider_id, "musicbrainz");
    assert_eq!(identity.source.external_id, MBID);
    assert_eq!(identity.source.match_basis, MatchBasis::Identifier);
    let artist_url = format!("https://musicbrainz.org/artist/{MBID}");
    assert_eq!(identity.source.url.as_deref(), Some(artist_url.as_str()));
    assert_eq!(identity.licence.id, "musicbrainz");
    assert!(!identity.licence.attribution_required);
    let page_urls = find(&document, FieldId::PageUrl);
    assert_eq!(
        page_urls
            .iter()
            .map(|candidate| candidate.source.url.as_deref().unwrap())
            .collect::<Vec<_>>(),
        [
            "https://www.coldplay.com",
            "https://en.wikipedia.org/wiki/Coldplay",
            "https://www.wikidata.org/wiki/Q11647",
            "https://www.discogs.com/artist/39116",
            "https://www.last.fm/music/Coldplay",
        ]
    );
    assert_eq!(
        page_urls
            .iter()
            .map(|candidate| candidate.rank)
            .collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2), Some(3), Some(4)]
    );
    assert!(
        page_urls
            .iter()
            .all(|candidate| candidate.source.provider_id == "musicbrainz")
    );
    assert!(
        page_urls
            .iter()
            .all(|candidate| candidate.licence.id == "musicbrainz")
    );
    let tags = find(&document, FieldId::Tag)[0];
    let FieldValue::Candidates(tags) = &tags.value else {
        panic!("tag candidates are stored as candidates")
    };
    assert_eq!(
        tags.iter().map(|tag| tag.name.as_str()).collect::<Vec<_>>(),
        ["rock", "alternative rock", "pop rock"]
    );
    assert_eq!(
        tags.iter().map(|tag| tag.rank).collect::<Vec<_>>(),
        [Some(0), Some(1), Some(2)]
    );
    assert_eq!(
        find(&document, FieldId::Tag)[0].licence.id,
        "musicbrainz.supplementary"
    );
    let portrait = find(&document, FieldId::ImagePortrait)[0];
    assert_eq!(portrait.source.provider_id, "commons");
    assert_eq!(portrait.source.external_id, "ColdplayWembley.jpg");
    assert_eq!(
        portrait.source.url.as_deref(),
        Some("https://commons.wikimedia.org/wiki/File:ColdplayWembley.jpg")
    );
    assert_eq!(portrait.image.as_ref().unwrap().role, "portrait");
    assert_eq!(
        portrait.value,
        FieldValue::Reference(
            "https://upload.wikimedia.org/wikipedia/commons/a/a1/ColdplayWembley.jpg".to_owned()
        )
    );
    assert_eq!(portrait.attribution.as_ref().unwrap().text, "Ricardo");
    assert_eq!(portrait.licence.id, "commons:CC BY 4.0");
    assert!(portrait.licence.attribution_required);
    assert!(!portrait.licence.share_alike);
    let summary = find(&document, FieldId::BiographySummary)[0];
    assert_eq!(
        summary.value,
        FieldValue::Text("Coldplay are a British rock band formed in London in 1997.".to_owned())
    );
    assert_eq!(summary.source.provider_id, "wikipedia");
    assert_eq!(summary.source.external_id, "Coldplay");
    assert_eq!(
        summary.source.url.as_deref(),
        Some("https://en.wikipedia.org/wiki/Coldplay")
    );
    assert_eq!(summary.licence.id, "wikipedia");
    let content = find(&document, FieldId::BiographyContent)[0];
    assert_eq!(content.source, summary.source);
    assert_eq!(content.licence, summary.licence);
    assert!(matches!(&content.value, FieldValue::Text(text) if text.contains("Chris Martin")));
    let language = find(&document, FieldId::BiographyLanguage)[0];
    assert_eq!(language.value, FieldValue::Text("en".to_owned()));
    assert_eq!(language.language.as_deref(), Some("en"));
    assert_eq!(language.licence, summary.licence);
    let calls = stub.calls.lock().unwrap();
    assert_eq!(calls.len(), 5);
    assert_eq!(
        calls
            .iter()
            .filter(|(uri, _)| uri.starts_with("/ws/2/artist/"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(uri, _)| uri.contains("action=wbgetentities"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(uri, _)| uri.contains("prop=imageinfo"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(uri, _)| uri.starts_with("/api/rest_v1/page/summary/"))
            .count(),
        1
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(uri, _)| uri.contains("prop=extracts"))
            .count(),
        1
    );
}

#[tokio::test]
async fn the_same_commons_file_from_the_image_rel_and_p18_is_not_merged() {
    let (base, stub) = start_stub(Vec::new()).await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &[FieldId::ImagePortrait])
        .await
        .unwrap();
    assert_eq!(find(&document, FieldId::ImagePortrait).len(), 1);
    assert_eq!(
        stub.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|(uri, _)| uri.contains("prop=imageinfo"))
            .count(),
        1
    );
}

#[tokio::test]
async fn a_commons_image_rel_is_the_portrait_without_a_wikidata_call() {
    let (base, stub) = start_stub(vec![(
        "/ws/2/artist/".to_owned(),
        reply(
            StatusCode::OK,
            include_str!("fixtures/controlled_musicbrainz_artist_document_commons_image_only.json"),
        ),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &[FieldId::ImagePortrait])
        .await
        .unwrap();
    assert_eq!(find(&document, FieldId::ImagePortrait).len(), 1);
    let calls = stub.calls.lock().unwrap();
    assert!(
        !calls
            .iter()
            .any(|(uri, _)| uri.contains("action=wbgetentities"))
    );
    assert_eq!(
        calls
            .iter()
            .filter(|(uri, _)| uri.contains("prop=imageinfo"))
            .count(),
        1
    );
}

#[tokio::test]
async fn a_non_commons_image_rel_falls_through_to_p18() {
    let artist: Value = serde_json::from_str(include_str!(
        "fixtures/controlled_musicbrainz_artist_document_non_commons_image.json"
    ))
    .unwrap();
    let relations = parse_relations(artist.as_object().unwrap()).unwrap();
    assert!(commons_image_names(&relations).is_empty());
    let wikidata: Value = serde_json::from_str(WIKIDATA).unwrap();
    assert!(
        !p18_file_names(&parse_wikidata_entity(&wikidata, "Q11647").unwrap())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn a_non_commons_image_rel_without_p18_yields_no_portrait() {
    let artist: Value = serde_json::from_str(include_str!(
        "fixtures/controlled_musicbrainz_artist_document_non_commons_image.json"
    ))
    .unwrap();
    let wikidata: Value = serde_json::from_str(include_str!(
        "fixtures/controlled_wikidata_entity_no_p18.json"
    ))
    .unwrap();
    assert!(commons_image_names(&parse_relations(artist.as_object().unwrap()).unwrap()).is_empty());
    assert!(
        p18_file_names(&parse_wikidata_entity(&wikidata, "Q11647").unwrap())
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn multiple_p18_claims_are_two_candidates_in_claim_order() {
    let (base, _) = start_stub(vec![
        (
            "/ws/2/artist/".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_musicbrainz_artist_document_wikidata_only.json"),
            ),
        ),
        (
            "action=wbgetentities".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikidata_entity_two_p18.json"),
            ),
        ),
        ("prop=imageinfo".to_owned(), reply(StatusCode::OK, COMMONS)),
        (
            "prop=imageinfo".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_commons_imageinfo_share_alike.json"),
            ),
        ),
    ])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &[FieldId::ImagePortrait])
        .await
        .unwrap();
    let portraits = find(&document, FieldId::ImagePortrait);
    assert_eq!(portraits.len(), 2);
    assert_eq!(
        portraits
            .iter()
            .map(|image| image.source.external_id.as_str())
            .collect::<Vec<_>>(),
        ["ColdplayWembley.jpg", "ColdplayMadrid.jpg"]
    );
    assert_eq!(
        portraits.iter().map(|image| image.rank).collect::<Vec<_>>(),
        [Some(0), Some(1)]
    );
    assert_eq!(
        portraits
            .iter()
            .map(|image| image.licence.id.as_str())
            .collect::<Vec<_>>(),
        ["commons:CC BY 4.0", "commons:CC BY-SA 4.0"]
    );
    assert_eq!(
        portraits
            .iter()
            .map(|image| image.licence.share_alike)
            .collect::<Vec<_>>(),
        [false, true]
    );
}

#[tokio::test]
async fn attribution_required_without_a_credit_stays_uncertain() {
    let commons: Value = serde_json::from_str(include_str!(
        "fixtures/controlled_commons_imageinfo_no_credit.json"
    ))
    .unwrap();
    let image = parse_commons_imageinfo(&commons, "ColdplayWembley.jpg")
        .unwrap()
        .unwrap();
    let candidate = image_candidate(&image, 0);
    assert!(image.required);
    assert!(image.attribution.is_none());
    assert!(candidate.attribution.is_none());
    assert!(candidate.licence.attribution_required);
    assert!(candidate.licence.share_alike);
    assert!(!candidate.licence.non_commercial);
}

#[tokio::test]
async fn a_cc0_file_needs_no_attribution() {
    let commons: Value = serde_json::from_str(include_str!(
        "fixtures/controlled_commons_imageinfo_cc0.json"
    ))
    .unwrap();
    let image = parse_commons_imageinfo(&commons, "ColdplayWembley.jpg")
        .unwrap()
        .unwrap();
    let candidate = image_candidate(&image, 0);
    assert!(!image.required);
    assert!(image.attribution.is_none());
    assert!(candidate.attribution.is_none());
    assert!(!candidate.licence.attribution_required);
    assert!(!candidate.licence.share_alike);
}

#[tokio::test]
async fn no_p18_and_no_image_rel_yields_no_portrait_and_no_error() {
    let (base, stub) = start_stub(vec![
        (
            "/ws/2/artist/".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_musicbrainz_artist_document_wikidata_only.json"),
            ),
        ),
        (
            "action=wbgetentities".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikidata_entity_no_p18.json"),
            ),
        ),
    ])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &[FieldId::ImagePortrait])
        .await
        .unwrap();
    assert!(find(&document, FieldId::ImagePortrait).is_empty());
    assert!(errors(&document, FieldId::ImagePortrait).is_empty());
    assert!(
        !stub
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(uri, _)| uri.contains("prop=imageinfo"))
    );
}

#[tokio::test]
async fn a_missing_wikidata_entity_is_absence_not_an_error() {
    let (base, stub) = start_stub(vec![
        (
            "/ws/2/artist/".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_musicbrainz_artist_document_wikidata_only.json"),
            ),
        ),
        (
            "action=wbgetentities".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikidata_entity_missing.json"),
            ),
        ),
    ])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert!(find(&document, FieldId::ImagePortrait).is_empty());
    assert!(find(&document, FieldId::BiographySummary).is_empty());
    assert!(document.errors.is_empty());
    let calls = stub.calls.lock().unwrap();
    assert!(!calls.iter().any(|(uri, _)| uri.contains("prop=imageinfo")));
    assert!(
        !calls
            .iter()
            .any(|(uri, _)| uri.starts_with("/api/rest_v1/"))
    );
}

#[tokio::test]
async fn a_wikidata_step_failure_only_fails_the_fields_it_blocked() {
    let (base, _) = start_stub(vec![(
        "action=wbgetentities".to_owned(),
        reply(StatusCode::SERVICE_UNAVAILABLE, "{}"),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert_eq!(errors(&document, FieldId::ImagePortrait).len(), 1);
    assert!(errors(&document, FieldId::BiographySummary).is_empty());
    assert!(!find(&document, FieldId::BiographySummary).is_empty());
}

#[tokio::test]
async fn a_wikidata_step_failure_without_a_wikipedia_rel_fails_the_biography() {
    let artist = include_str!("fixtures/controlled_musicbrainz_artist_document_wikidata_only.json")
        .to_owned();
    let (base, _) = start_stub(vec![
        ("/ws/2/artist/".to_owned(), reply(StatusCode::OK, artist)),
        (
            "action=wbgetentities".to_owned(),
            reply(StatusCode::SERVICE_UNAVAILABLE, "{}"),
        ),
    ])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert_eq!(errors(&document, FieldId::ImagePortrait).len(), 1);
    assert_eq!(errors(&document, FieldId::BiographySummary).len(), 1);
    assert_eq!(errors(&document, FieldId::BiographyContent).len(), 1);
    assert_eq!(errors(&document, FieldId::BiographyLanguage).len(), 1);
}

#[tokio::test]
async fn a_wikipedia_summary_failure_is_one_field_error() {
    let (base, _) = start_stub(vec![(
        "/api/rest_v1".to_owned(),
        reply(StatusCode::SERVICE_UNAVAILABLE, "{}"),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert_eq!(errors(&document, FieldId::BiographySummary).len(), 1);
    assert!(errors(&document, FieldId::BiographyContent).is_empty());
    assert!(!find(&document, FieldId::BiographyContent).is_empty());
}

#[tokio::test]
async fn a_commons_step_failure_is_a_portrait_field_error() {
    let (base, _) = start_stub(vec![(
        "prop=imageinfo".to_owned(),
        reply(StatusCode::SERVICE_UNAVAILABLE, "{}"),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &[FieldId::ImagePortrait])
        .await
        .unwrap();
    assert_eq!(errors(&document, FieldId::ImagePortrait).len(), 1);
    assert!(errors(&document, FieldId::ImagePortrait)[0].retryable);
}

#[tokio::test]
async fn a_wrong_typed_wikidata_claims_member_is_a_portrait_field_error() {
    let (base, _) = start_stub(vec![(
        "action=wbgetentities".to_owned(),
        reply(
            StatusCode::OK,
            include_str!("fixtures/controlled_wikidata_entity_wrong_claims_type.json"),
        ),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert_eq!(errors(&document, FieldId::ImagePortrait).len(), 1);
    assert_eq!(
        errors(&document, FieldId::ImagePortrait)[0].code,
        "resolution_failed"
    );
    assert!(!find(&document, FieldId::BiographySummary).is_empty());
}

#[tokio::test]
async fn a_wrong_typed_extract_member_is_a_content_field_error() {
    let (base, _) = start_stub(vec![(
        "prop=extracts".to_owned(),
        reply(
            StatusCode::OK,
            include_str!("fixtures/controlled_wikipedia_extract_wrong_page_type.json"),
        ),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert_eq!(errors(&document, FieldId::BiographyContent).len(), 1);
    assert!(!find(&document, FieldId::BiographySummary).is_empty());
}

#[tokio::test]
async fn a_disambiguation_page_yields_no_biography_candidate() {
    let (base, stub) = start_stub(vec![(
        "/api/rest_v1".to_owned(),
        reply(
            StatusCode::OK,
            include_str!("fixtures/controlled_wikipedia_summary_disambiguation.json"),
        ),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert!(find(&document, FieldId::BiographySummary).is_empty());
    assert!(find(&document, FieldId::BiographyContent).is_empty());
    assert!(
        !stub
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|(uri, _)| uri.contains("prop=extracts"))
    );
}

#[tokio::test]
async fn an_empty_extract_yields_no_biography_placeholder() {
    let (base, _) = start_stub(vec![
        (
            "/api/rest_v1".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikipedia_summary_no_extract.json"),
            ),
        ),
        (
            "prop=extracts".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikipedia_extract_empty.json"),
            ),
        ),
    ])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert!(find(&document, FieldId::BiographySummary).is_empty());
    assert!(find(&document, FieldId::BiographyContent).is_empty());
    assert!(find(&document, FieldId::BiographyLanguage).is_empty());
}

#[tokio::test]
async fn biography_language_comes_from_the_wiki_host() {
    let artist = include_str!("fixtures/controlled_musicbrainz_artist_document_german_wiki.json");
    let (base, stub) = start_stub(vec![
        ("/ws/2/artist/".to_owned(), reply(StatusCode::OK, artist)),
        (
            "/api/rest_v1".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikipedia_summary_german.json"),
            ),
        ),
        (
            "prop=extracts".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikipedia_extract_german.json"),
            ),
        ),
    ])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert_eq!(
        find(&document, FieldId::BiographyLanguage)[0]
            .language
            .as_deref(),
        Some("de")
    );
    assert!(
        stub.calls.lock().unwrap()[3]
            .0
            .starts_with("/api/rest_v1/page/summary/")
    );
}

#[tokio::test]
async fn a_wikidata_sitelink_supplies_the_wiki_target() {
    let artist = include_str!("fixtures/controlled_musicbrainz_artist_document_wikidata_only.json");
    let (base, stub) = start_stub(vec![
        ("/ws/2/artist/".to_owned(), reply(StatusCode::OK, artist)),
        (
            "action=wbgetentities".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikidata_entity_german_sitelink.json"),
            ),
        ),
        (
            "/api/rest_v1".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikipedia_summary_german.json"),
            ),
        ),
        (
            "prop=extracts".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_wikipedia_extract_german.json"),
            ),
        ),
    ])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "de", &[FieldId::BiographyLanguage])
        .await
        .unwrap();
    assert_eq!(
        find(&document, FieldId::BiographyLanguage)[0]
            .language
            .as_deref(),
        Some("de")
    );
    assert!(
        stub.calls.lock().unwrap()[2]
            .0
            .starts_with("/api/rest_v1/page/summary/")
    );
}

#[tokio::test]
async fn an_over_cap_summary_is_omitted_never_clamped() {
    let mut summary: Value = serde_json::from_str(SUMMARY).unwrap();
    summary["extract"] = json!("a".repeat(4097));
    let (base, _) = start_stub(vec![(
        "/api/rest_v1".to_owned(),
        reply(StatusCode::OK, summary.to_string()),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert!(find(&document, FieldId::BiographySummary).is_empty());
    assert_eq!(
        errors(&document, FieldId::BiographySummary)[0].code,
        "resolution_failed"
    );
}

#[tokio::test]
async fn an_over_cap_extract_is_omitted_never_clamped() {
    let mut extract: Value = serde_json::from_str(EXTRACT).unwrap();
    extract["query"]["pages"]["1"]["extract"] = json!("a".repeat(65537));
    let (base, _) = start_stub(vec![(
        "prop=extracts".to_owned(),
        reply(StatusCode::OK, extract.to_string()),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &fields())
        .await
        .unwrap();
    assert!(find(&document, FieldId::BiographyContent).is_empty());
    assert_eq!(
        errors(&document, FieldId::BiographyContent)[0].code,
        "resolution_failed"
    );
}

#[tokio::test]
async fn a_summary_at_the_cap_is_kept() {
    let mut summary: Value = serde_json::from_str(SUMMARY).unwrap();
    summary["extract"] = json!("a".repeat(4096));
    let (base, _) = start_stub(vec![(
        "/api/rest_v1".to_owned(),
        reply(StatusCode::OK, summary.to_string()),
    )])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &[FieldId::BiographySummary])
        .await
        .unwrap();
    assert_eq!(
        find(&document, FieldId::BiographySummary)[0].value,
        FieldValue::Text("a".repeat(4096))
    );
    assert!(document.errors.is_empty());
}

#[test]
fn tags_are_capped_at_32_entries_and_128_bytes_each() {
    let tags = (0..40)
        .map(|index| json!({"name":format!("tag{index}")}))
        .collect::<Vec<_>>();
    let artist = json!({"tags": [{"name":"x".repeat(129)}], "genres": tags});
    let candidate = tag_candidate(&source(), artist.as_object().unwrap())
        .unwrap()
        .unwrap();
    let FieldValue::Candidates(values) = candidate.value else {
        panic!("tag values are candidates")
    };
    assert_eq!(values.len(), 32);
    assert_eq!(values[0].name, "tag0");
    assert_eq!(values[31].name, "tag31");
}

#[tokio::test]
async fn only_the_requested_fields_cost_a_chain_call() {
    let (base, stub) = start_stub(Vec::new()).await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &[FieldId::IdentityMbid])
        .await
        .unwrap();
    assert_eq!(find(&document, FieldId::IdentityMbid).len(), 1);
    assert_eq!(stub.calls.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn a_document_without_relations_or_tags_is_identity_only() {
    let payload = json!({"id":MBID,"name":"Coldplay"});
    let parsed = parse_artist(&payload, MBID).unwrap();
    let relations = parse_relations(parsed).unwrap();
    assert!(relations.is_empty());
    assert!(tag_candidate(&source(), parsed).unwrap().is_none());
}

#[tokio::test]
async fn a_missing_commons_file_is_a_portrait_field_error() {
    let (base, _) = start_stub(vec![
        (
            "/ws/2/artist/".to_owned(),
            reply(
                StatusCode::OK,
                include_str!(
                    "fixtures/controlled_musicbrainz_artist_document_commons_image_only.json"
                ),
            ),
        ),
        (
            "prop=imageinfo".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_commons_imageinfo_missing.json"),
            ),
        ),
    ])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &[FieldId::ImagePortrait])
        .await
        .unwrap();
    assert!(find(&document, FieldId::ImagePortrait).is_empty());
    assert_eq!(errors(&document, FieldId::ImagePortrait).len(), 1);
    assert_eq!(
        errors(&document, FieldId::ImagePortrait)[0].code,
        "resolution_failed"
    );
    assert!(!errors(&document, FieldId::ImagePortrait)[0].retryable);
}

#[tokio::test]
async fn a_commons_response_for_another_file_is_not_attributed() {
    let (base, _) = start_stub(vec![
        (
            "/ws/2/artist/".to_owned(),
            reply(
                StatusCode::OK,
                include_str!(
                    "fixtures/controlled_musicbrainz_artist_document_commons_image_only.json"
                ),
            ),
        ),
        (
            "prop=imageinfo".to_owned(),
            reply(
                StatusCode::OK,
                include_str!("fixtures/controlled_commons_imageinfo_other_title.json"),
            ),
        ),
    ])
    .await;
    let document = MusicBrainzDocuments::with_client(Arc::new(client(&base)))
        .fetch(&source(), "en", &[FieldId::ImagePortrait])
        .await
        .unwrap();
    assert!(find(&document, FieldId::ImagePortrait).is_empty());
    assert_eq!(errors(&document, FieldId::ImagePortrait).len(), 1);
    assert_eq!(
        errors(&document, FieldId::ImagePortrait)[0].code,
        "resolution_failed"
    );
    assert!(!errors(&document, FieldId::ImagePortrait)[0].retryable);
}

#[tokio::test]
async fn a_malformed_musicbrainz_document_is_a_resolution_failure() {
    for payload in [
        json!([]),
        json!({"name":"Coldplay"}),
        json!({"id":""}),
        json!({"id":"other"}),
    ] {
        assert!(matches!(
            parse_artist(&payload, MBID),
            Err(DocumentError::ResolutionFailed)
        ));
    }
    assert!(matches!(
        parse_relations(json!({"relations":false}).as_object().unwrap()),
        Err(DocumentError::ResolutionFailed)
    ));
    assert!(matches!(
        tag_candidate(&source(), json!({"tags":false}).as_object().unwrap()),
        Err(DocumentError::ResolutionFailed)
    ));
}

#[test]
fn the_controlled_fixtures_carry_no_credential_or_signed_url() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src/integrations/musicbrainz/fixtures");
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if !path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("controlled_")
        {
            continue;
        }
        let fixture = std::fs::read_to_string(path).unwrap();
        let value = fixture.to_ascii_lowercase();
        for marker in [
            "token=",
            "sig=",
            "signature=",
            "x-amz-",
            "bearer ",
            "api_key",
            "api-key",
            "authorization:",
            "password",
            "secret",
            "sentinel",
        ] {
            assert!(!value.contains(marker), "fixture contained {marker}");
        }
    }
}
