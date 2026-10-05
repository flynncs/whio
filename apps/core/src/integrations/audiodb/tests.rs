use super::*;
use crate::features::metadata::{
    domain::{FieldValue, MatchBasis},
    refresh::{DocumentCandidate, DocumentFieldError},
};
use axum::{
    Router,
    body::Body,
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

use crate::platform::pacing::CallLimiter;

const FIXTURE: &str = include_str!("fixtures/audiodb_artist_document.json");
const LANGUAGES: &str = include_str!("fixtures/audiodb_artist_document_language_member.json");
const EMPTY: &str = include_str!("fixtures/audiodb_artist_document_empty_biography.json");
const WIDE: &str = include_str!("fixtures/audiodb_artist_document_wide_only.json");
const CREATIVE: &str = include_str!("fixtures/audiodb_artist_document_creative_commons.json");
const MBID: &str = "cc197bad-dc9c-440d-a5b5-d52ba2e14234";
const THUMBNAIL: &str = "https://r2.theaudiodb.com/images/media/artist/thumb/uxrqxy1347913147.jpg";
const WIDE_THUMBNAIL: &str =
    "https://r2.theaudiodb.com/images/media/artist/widethumb/sxqspt1516190718.jpg";

#[derive(Clone)]
struct Reply {
    status: StatusCode,
    body: String,
    retry_after: Option<String>,
}

#[derive(Clone)]
struct Stub {
    requests: Arc<Mutex<Vec<String>>>,
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
        requests: Arc::new(Mutex::new(Vec::new())),
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
    state.requests.lock().unwrap().push(uri.clone());
    let selected = {
        let mut replies = state.replies.lock().unwrap();
        replies
            .iter()
            .position(|(pattern, _)| uri.contains(pattern))
            .and_then(|index| replies.remove(index))
            .map(|(_, response)| response)
    }
    .unwrap_or_else(|| reply(StatusCode::OK, FIXTURE));
    let mut builder = Response::builder().status(selected.status);
    if let Some(retry_after) = selected.retry_after {
        builder = builder.header("retry-after", retry_after);
    }
    builder.body(Body::from(selected.body)).unwrap()
}

fn client(base: &Url, key: &str) -> client::AudioDbClient {
    client::AudioDbClient::with_config(
        base.clone(),
        key.to_owned(),
        Arc::new(CallLimiter::new(Duration::ZERO)),
    )
    .unwrap()
}

fn source() -> SourceIdentity {
    SourceIdentity::new(
        crate::features::music_library::domain::ProviderId::new("theaudiodb".to_owned()).unwrap(),
        crate::features::music_library::domain::SourceScope::Global,
        MBID.to_owned(),
    )
    .unwrap()
}

fn artist_fixture() -> Value {
    serde_json::from_str::<Value>(FIXTURE).unwrap()["artists"][0].clone()
}

fn document(artist: &Value, language: &str, fields: &[FieldId]) -> DocumentOutcome {
    let source = source();
    super::artist::document(
        &source,
        artist.as_object().unwrap(),
        language,
        fields,
        chrono::Utc::now(),
    )
    .unwrap()
}

fn fields(result: &DocumentOutcome) -> Vec<FieldId> {
    result
        .candidates
        .iter()
        .map(|candidate| candidate.field)
        .collect()
}

fn candidate(result: &DocumentOutcome, field: FieldId) -> &DocumentCandidate {
    result
        .candidates
        .iter()
        .find(|candidate| candidate.field == field)
        .unwrap()
}

fn error(result: &DocumentOutcome, field: FieldId) -> &DocumentFieldError {
    result
        .errors
        .iter()
        .find(|error| error.field == field)
        .unwrap()
}

#[tokio::test]
async fn the_artist_mbid_route_is_requested_once_with_the_configured_key() {
    let (base, stub) = start_stub(Vec::new()).await;
    let payload = client(&base, "injected-key")
        .fetch_artist_by_mbid(MBID)
        .await
        .unwrap();
    assert_eq!(payload["artists"][0]["strMusicBrainzID"], MBID);
    assert_eq!(
        stub.requests.lock().unwrap().as_slice(),
        [format!("/api/v1/json/injected-key/artist-mb.php?i={MBID}")]
    );
}

#[tokio::test]
async fn a_429_propagates_the_retry_after_delay_as_unavailable() {
    let mut response = reply(StatusCode::TOO_MANY_REQUESTS, "{} ");
    response.retry_after = Some("7".to_owned());
    let (base, _) = start_stub(vec![("artist-mb.php".to_owned(), response)]).await;
    assert_eq!(
        client(&base, "key")
            .fetch_artist_by_mbid(MBID)
            .await
            .unwrap_err(),
        DocumentError::Unavailable {
            retry_after: Some(Duration::from_secs(7))
        }
    );
}

#[tokio::test]
async fn server_failures_are_unavailable_and_retryable() {
    let (base, _) = start_stub(vec![(
        "artist-mb.php".to_owned(),
        reply(StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
    )])
    .await;
    assert_eq!(
        client(&base, "key")
            .fetch_artist_by_mbid(MBID)
            .await
            .unwrap_err(),
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
        client(&closed, "key")
            .fetch_artist_by_mbid(MBID)
            .await
            .unwrap_err(),
        DocumentError::Unavailable { retry_after: None }
    );
}

#[tokio::test]
async fn a_non_json_body_is_a_resolution_failure() {
    let (base, _) = start_stub(vec![(
        "artist-mb.php".to_owned(),
        reply(StatusCode::OK, "<html>not json</html>"),
    )])
    .await;
    assert_eq!(
        client(&base, "key")
            .fetch_artist_by_mbid(MBID)
            .await
            .unwrap_err(),
        DocumentError::ResolutionFailed
    );
}

#[tokio::test]
async fn a_client_error_is_a_resolution_failure() {
    let (base, _) = start_stub(vec![(
        "artist-mb.php".to_owned(),
        reply(StatusCode::BAD_REQUEST, "bad request"),
    )])
    .await;
    assert_eq!(
        client(&base, "key")
            .fetch_artist_by_mbid(MBID)
            .await
            .unwrap_err(),
        DocumentError::ResolutionFailed
    );
}

#[tokio::test]
async fn the_key_is_not_logged_or_echoed_in_an_error() {
    let secret = "sentinel-audiodb-key-value";
    let (base, _) = start_stub(vec![(
        "artist-mb.php".to_owned(),
        reply(StatusCode::BAD_REQUEST, "private transport details"),
    )])
    .await;
    let error = client(&base, secret)
        .fetch_artist_by_mbid(MBID)
        .await
        .unwrap_err();
    assert_eq!(error, DocumentError::ResolutionFailed);
    assert!(!error.to_string().contains(secret));
    assert!(!format!("{error:?}").contains(secret));
    assert!(!error.to_string().contains("private transport details"));
}

#[test]
fn configured_key_reads_the_resolver_environment() {
    let vars = crate::platform::config::Vars::from([
        ("WHIO_AUDIODB_ENABLED".to_owned(), "true".to_owned()),
        ("WHIO_AUDIODB_KEY".to_owned(), "  resolved-key  ".to_owned()),
    ]);
    assert_eq!(
        config::from_vars(&vars).unwrap().unwrap().key,
        "resolved-key"
    );
    let vars = crate::platform::config::Vars::from([(
        "WHIO_AUDIODB_ENABLED".to_owned(),
        "false".to_owned(),
    )]);
    assert!(config::from_vars(&vars).unwrap().is_none());
}

#[test]
fn one_document_yields_biography_portrait_and_wide_from_a_single_fetch() {
    let result = document(
        &artist_fixture(),
        "en",
        &[
            FieldId::BiographyContent,
            FieldId::ImagePortrait,
            FieldId::ImageWide,
        ],
    );
    assert_eq!(
        fields(&result),
        [
            FieldId::BiographyContent,
            FieldId::ImagePortrait,
            FieldId::ImageWide
        ]
    );
    assert_eq!(result.address, source());
    assert!(result.errors.is_empty());
    let biography = candidate(&result, FieldId::BiographyContent);
    assert_eq!(
        biography.value,
        FieldValue::Text(
            artist_fixture()["strBiography"]
                .as_str()
                .unwrap()
                .to_owned()
        )
    );
    assert_eq!(biography.licence.id, "theaudiodb");
    assert!(!biography.licence.redistribution_allowed);
    assert!(biography.licence.cache_allowed);
    assert_eq!(
        biography.attribution.as_ref().unwrap().text,
        "Data provided by TheAudioDB"
    );
    assert_eq!(biography.source.provider_id, "theaudiodb");
    assert_eq!(biography.source.external_id, MBID);
    assert_eq!(biography.source.match_basis, MatchBasis::EstablishedSource);
    assert_eq!(
        biography.source.url.as_deref(),
        Some("https://www.theaudiodb.com/artist/111239")
    );
    let portrait = candidate(&result, FieldId::ImagePortrait);
    assert_eq!(portrait.image.as_ref().unwrap().role, "portrait");
    assert_eq!(portrait.value, FieldValue::Reference(THUMBNAIL.to_owned()));
    assert_eq!(portrait.image.as_ref().unwrap().width, None);
    assert_eq!(portrait.licence.id, "theaudiodb");
    let wide = candidate(&result, FieldId::ImageWide);
    assert_eq!(wide.image.as_ref().unwrap().role, "wide");
    assert_eq!(wide.value, FieldValue::Reference(WIDE_THUMBNAIL.to_owned()));
}

#[tokio::test]
async fn the_document_is_fetched_exactly_once_however_many_fields_are_asked_for() {
    let (base, stub) = start_stub(Vec::new()).await;
    let docs = AudioDbDocuments::with_config(
        base,
        "key".to_owned(),
        Arc::new(CallLimiter::new(Duration::ZERO)),
    )
    .unwrap();
    docs.fetch(
        &source(),
        &MetadataProfile::artist_display(),
        "en",
        &[
            FieldId::BiographyContent,
            FieldId::ImagePortrait,
            FieldId::ImageWide,
        ],
    )
    .await
    .unwrap();
    assert_eq!(stub.requests.lock().unwrap().len(), 1);
}

#[test]
fn the_requested_language_selects_the_biography_member() {
    let artist = serde_json::from_str::<Value>(LANGUAGES).unwrap()["artists"][0].clone();
    for (language, key, declared) in [
        ("en", "strBiographyEN", "en"),
        ("de", "strBiographyDE", "de"),
        ("", "strBiographyEN", "en"),
    ] {
        let result = document(
            &artist,
            language,
            &[FieldId::BiographyContent, FieldId::BiographyLanguage],
        );
        assert_eq!(
            candidate(&result, FieldId::BiographyContent).value,
            FieldValue::Text(artist[key].as_str().unwrap().trim().to_owned())
        );
        assert_eq!(
            candidate(&result, FieldId::BiographyLanguage).value,
            FieldValue::Text(declared.to_owned())
        );
        assert_eq!(
            candidate(&result, FieldId::BiographyLanguage)
                .language
                .as_deref(),
            Some(declared)
        );
    }
}

#[test]
fn empty_biography_omits_the_candidate_without_an_error() {
    let artist = serde_json::from_str::<Value>(EMPTY).unwrap()["artists"][0].clone();
    let result = document(
        &artist,
        "en",
        &[FieldId::BiographyContent, FieldId::ImagePortrait],
    );
    assert_eq!(fields(&result), [FieldId::ImagePortrait]);
    assert!(result.errors.is_empty());
}

#[test]
fn biography_over_the_cap_is_omitted_and_reported() {
    let mut artist = artist_fixture();
    artist["strBiographyEN"] = json!("a".repeat(65_537));
    let result = document(
        &artist,
        "en",
        &[FieldId::BiographyContent, FieldId::ImagePortrait],
    );
    assert!(!fields(&result).contains(&FieldId::BiographyContent));
    assert_eq!(
        error(&result, FieldId::BiographyContent).code,
        "resolution_failed"
    );
    assert!(!error(&result, FieldId::BiographyContent).retryable);
}

#[test]
fn wide_falls_back_to_the_fanart_member() {
    let artist = serde_json::from_str::<Value>(WIDE).unwrap()["artists"][0].clone();
    let result = document(&artist, "en", &[FieldId::ImagePortrait, FieldId::ImageWide]);
    assert_eq!(fields(&result), [FieldId::ImageWide]);
    assert_eq!(
        candidate(&result, FieldId::ImageWide).value,
        FieldValue::Reference(artist["strArtistFanart"].as_str().unwrap().to_owned())
    );
}

#[test]
fn a_wrong_typed_content_member_is_a_field_error_that_leaves_the_other_field() {
    let mut artist = artist_fixture();
    artist["strArtistThumb"] = json!(17);
    let result = document(
        &artist,
        "en",
        &[FieldId::BiographyContent, FieldId::ImagePortrait],
    );
    assert_eq!(fields(&result), [FieldId::BiographyContent]);
    assert_eq!(
        error(&result, FieldId::ImagePortrait).code,
        "resolution_failed"
    );
    assert!(!error(&result, FieldId::ImagePortrait).retryable);
}

#[test]
fn creative_commons_artwork_changes_only_the_artwork_licence() {
    let artist = serde_json::from_str::<Value>(CREATIVE).unwrap()["artists"][0].clone();
    let result = document(
        &artist,
        "en",
        &[FieldId::BiographyContent, FieldId::ImagePortrait],
    );
    assert_eq!(
        candidate(&result, FieldId::BiographyContent).licence.id,
        "theaudiodb"
    );
    let artwork = candidate(&result, FieldId::ImagePortrait);
    assert_eq!(artwork.licence.id, "commons:CC BY 4.0");
    assert!(artwork.licence.redistribution_allowed);
}

#[test]
fn a_creative_commons_string_signal_is_honoured() {
    let mut artist = serde_json::from_str::<Value>(CREATIVE).unwrap()["artists"][0].clone();
    artist["strCreativeCommons"] = json!("Yes");
    let result = document(&artist, "en", &[FieldId::ImagePortrait]);
    assert_eq!(
        candidate(&result, FieldId::ImagePortrait).licence.id,
        "commons:CC BY 4.0"
    );
}

#[test]
fn a_document_without_the_artist_page_id_omits_the_source_url() {
    let mut artist = artist_fixture();
    artist.as_object_mut().unwrap().remove("idArtist");
    let result = document(&artist, "en", &[FieldId::ImagePortrait]);
    assert_eq!(candidate(&result, FieldId::ImagePortrait).source.url, None);
}

#[test]
fn a_document_without_the_mbid_echo_cannot_name_its_producing_source() {
    let mut artist = artist_fixture();
    artist.as_object_mut().unwrap().remove("strMusicBrainzID");
    assert_eq!(
        super::artist::document(
            &source(),
            artist.as_object().unwrap(),
            "en",
            &[FieldId::ImagePortrait],
            chrono::Utc::now()
        )
        .unwrap_err(),
        DocumentError::ResolutionFailed
    );
}

#[test]
fn an_unknown_mbid_is_a_typed_not_found() {
    let payload = serde_json::from_str::<Value>(include_str!(
        "fixtures/audiodb_artist_document_missing.json"
    ))
    .unwrap();
    assert_eq!(
        super::artist::parse_artist(&payload).unwrap_err(),
        DocumentError::NotFound
    );
}

#[test]
fn structural_malformations_are_resolution_failures() {
    for payload in [
        serde_json::from_str::<Value>(include_str!(
            "fixtures/audiodb_artist_document_wrong_payload_type.json"
        ))
        .unwrap(),
        serde_json::from_str::<Value>(include_str!(
            "fixtures/audiodb_artist_document_wrong_collection_member_type.json"
        ))
        .unwrap(),
        json!({"artists":["not an artist record"]}),
        json!({"artists":[5]}),
    ] {
        assert_eq!(
            super::artist::parse_artist(&payload).unwrap_err(),
            DocumentError::ResolutionFailed
        );
    }
}

#[test]
fn an_absent_collection_member_is_a_resolution_failure() {
    assert_eq!(
        super::artist::parse_artist(&json!({})).unwrap_err(),
        DocumentError::ResolutionFailed
    );
}
