use chrono::Utc;
use serde_json::Value;

use super::documents::ListenBrainzClient;
use crate::features::{
    metadata::{
        domain::{Attribution, CandidateItem, FieldId, FieldValue, MatchBasis, MetricFact},
        licences::LicenceAssertion,
        refresh::{CandidateSource, DocumentCandidate, DocumentError, DocumentOutcome},
    },
    music_library::domain::SourceIdentity,
};
use crate::platform::pacing::CallLimiter;

const PROVIDER: &str = "listenbrainz";
const TOP_TRACKS_SCOPE: &str = "listenbrainz.popularity.top-recordings-for-artist";
const LICENSE: &str = "https://listenbrainz.readthedocs.io/en/latest/users/api/index.html";
type ArtistPopularity = Option<(Option<u64>, Option<u64>)>;

pub struct ListenBrainzDocuments {
    client: ListenBrainzClient,
}

impl ListenBrainzDocuments {
    pub fn new(
        config: &super::documents_config::Config,
        limiter: std::sync::Arc<CallLimiter>,
    ) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: ListenBrainzClient::new(config, limiter)?,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_client(client: ListenBrainzClient) -> Self {
        Self { client }
    }

    pub async fn fetch_document(
        &self,
        source: &SourceIdentity,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        let retrieved_at = Utc::now();
        let mut candidates = Vec::new();
        let mut errors = Vec::new();
        if fields.contains(&FieldId::Similar) {
            match self
                .client
                .fetch_similar_artists(source.external_id())
                .await
                .and_then(|payload| parse_similar_artists(&payload, source.external_id()))
            {
                Ok(items) if !items.is_empty() => candidates.push(candidate(
                    FieldId::Similar,
                    source.external_id(),
                    FieldValue::Candidates(items),
                    Some(self.client.algorithm().to_owned()),
                )),
                Ok(_) => {}
                Err(error) => errors.push(crate::integrations::document_http::field_error(
                    FieldId::Similar,
                    &error,
                )),
            }
        }
        if fields.contains(&FieldId::TopTracks) {
            match self
                .client
                .fetch_top_recordings(source.external_id())
                .await
                .and_then(|payload| parse_recordings(&payload))
            {
                Ok(items) if !items.is_empty() => candidates.push(candidate(
                    FieldId::TopTracks,
                    source.external_id(),
                    FieldValue::Candidates(items),
                    Some(TOP_TRACKS_SCOPE.to_owned()),
                )),
                Ok(_) => {}
                Err(error) => errors.push(crate::integrations::document_http::field_error(
                    FieldId::TopTracks,
                    &error,
                )),
            }
        }
        if fields.contains(&FieldId::MetricListen) || fields.contains(&FieldId::MetricListener) {
            match self
                .client
                .fetch_artist_popularity(source.external_id())
                .await
                .and_then(|payload| parse_artist_popularity(&payload, source.external_id()))
            {
                Ok(Some((listen, users))) => {
                    if fields.contains(&FieldId::MetricListen)
                        && let Some(count) = listen
                    {
                        candidates.push(metric_candidate(
                            FieldId::MetricListen,
                            source.external_id(),
                            count,
                            "listens",
                        ));
                    }
                    if fields.contains(&FieldId::MetricListener)
                        && let Some(count) = users
                    {
                        candidates.push(metric_candidate(
                            FieldId::MetricListener,
                            source.external_id(),
                            count,
                            "listeners",
                        ));
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    for field in [FieldId::MetricListen, FieldId::MetricListener] {
                        if fields.contains(&field) {
                            errors.push(crate::integrations::document_http::field_error(
                                field, &error,
                            ));
                        }
                    }
                }
            }
        }
        Ok(DocumentOutcome {
            address: source.clone(),
            source_revision: None,
            retrieved_at,
            candidates,
            errors,
        })
    }
}

fn candidate(
    field: FieldId,
    external_id: &str,
    value: FieldValue,
    scope: Option<String>,
) -> DocumentCandidate {
    let mut items = match value {
        FieldValue::Candidates(items) => items,
        _ => Vec::new(),
    };
    for item in &mut items {
        item.ranking_scope = scope.clone();
    }
    let source = source(external_id);
    let field_value = FieldValue::Candidates(items);
    DocumentCandidate {
        field,
        source,
        value: field_value,
        language: None,
        licence: licence(),
        attribution: Some(attribution()),
        source_url: None,
        rank: None,
        score: None,
        image: None,
        metric: None,
    }
}

pub(crate) fn parse_similar_artists(
    payload: &Value,
    mbid: &str,
) -> Result<Vec<CandidateItem>, DocumentError> {
    let entries = payload.as_array().ok_or(DocumentError::ResolutionFailed)?;
    let mut result = Vec::new();
    for entry in entries.iter().take(50) {
        let entry = entry.as_object().ok_or(DocumentError::ResolutionFailed)?;
        if let Some(reference) = entry.get("reference_mbid")
            && !reference.is_null()
            && reference.as_str() != Some(mbid)
        {
            return Err(DocumentError::ResolutionFailed);
        }
        let id = required_text(entry.get("artist_mbid"))?;
        let name = required_text(entry.get("name"))?;
        let score = optional_score(entry.get("score"))?;
        result.push(CandidateItem {
            name,
            external_id: Some(id),
            url: None,
            rank: Some((result.len()) as u32),
            score,
            artists: Vec::new(),
            release_name: None,
            duration_ms: None,
            isrc: None,
            recording_id: None,
            ranking_scope: None,
        });
    }
    Ok(result)
}

pub(crate) fn parse_recordings(payload: &[Value]) -> Result<Vec<CandidateItem>, DocumentError> {
    let mut result = Vec::new();
    for entry in payload {
        let entry = entry.as_object().ok_or(DocumentError::ResolutionFailed)?;
        let id = required_text(entry.get("recording_mbid"))?;
        let name = optional_text(entry.get("recording_name"))?;
        let Some(name) = name else {
            continue;
        };
        let listens = optional_count(entry.get("total_listen_count"))?;
        let users = optional_count(entry.get("total_user_count"))?;
        result.push(CandidateItem {
            name,
            external_id: Some(id),
            url: None,
            rank: listens.and_then(|count| u32::try_from(count).ok()),
            score: users.map(|count| count as f64),
            artists: Vec::new(),
            release_name: None,
            duration_ms: None,
            isrc: None,
            recording_id: None,
            ranking_scope: Some(TOP_TRACKS_SCOPE.to_owned()),
        });
        if result.len() == 50 {
            break;
        }
    }
    Ok(result)
}

pub(crate) fn parse_artist_popularity(
    payload: &Value,
    mbid: &str,
) -> Result<ArtistPopularity, DocumentError> {
    let entries = payload.as_array().ok_or(DocumentError::ResolutionFailed)?;
    let Some(entry) = entries.first() else {
        return Ok(None);
    };
    let entry = entry.as_object().ok_or(DocumentError::ResolutionFailed)?;
    if required_text(entry.get("artist_mbid"))? != mbid {
        return Err(DocumentError::ResolutionFailed);
    }
    Ok(Some((
        optional_count(entry.get("total_listen_count"))?,
        optional_count(entry.get("total_user_count"))?,
    )))
}

fn metric_candidate(
    field: FieldId,
    external_id: &str,
    count: u64,
    unit: &str,
) -> DocumentCandidate {
    DocumentCandidate {
        field,
        source: source(external_id),
        value: FieldValue::Count(count),
        language: None,
        licence: licence(),
        attribution: None,
        source_url: None,
        rank: None,
        score: None,
        image: None,
        metric: Some(MetricFact {
            unit: unit.to_owned(),
            population: Some("ListenBrainz users".to_owned()),
            period: None,
            as_of: None,
        }),
    }
}

fn source(external_id: &str) -> CandidateSource {
    CandidateSource {
        provider_id: PROVIDER.to_owned(),
        external_id: external_id.to_owned(),
        url: None,
        match_basis: MatchBasis::Identifier,
    }
}
fn attribution() -> Attribution {
    Attribution {
        text: "ListenBrainz".to_owned(),
        url: Some("https://listenbrainz.org".to_owned()),
    }
}
fn licence() -> LicenceAssertion {
    LicenceAssertion {
        id: "listenbrainz".to_owned(),
        url: Some(LICENSE.to_owned()),
        attribution_required: true,
        redistribution_allowed: false,
        cache_allowed: true,
        share_alike: false,
        non_commercial: false,
        storage_cap_bytes: None,
    }
}
fn required_text(value: Option<&Value>) -> Result<String, DocumentError> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .ok_or(DocumentError::ResolutionFailed)
}
fn optional_text(value: Option<&Value>) -> Result<Option<String>, DocumentError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => {
            Ok((!value.trim().is_empty()).then(|| value.trim().to_owned()))
        }
        Some(_) => Err(DocumentError::ResolutionFailed),
    }
}
fn optional_count(value: Option<&Value>) -> Result<Option<u64>, DocumentError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_u64()
            .map(Some)
            .ok_or(DocumentError::ResolutionFailed),
        Some(_) => Err(DocumentError::ResolutionFailed),
    }
}
fn optional_score(value: Option<&Value>) -> Result<Option<f64>, DocumentError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(value)) => value
            .as_f64()
            .map(Some)
            .ok_or(DocumentError::ResolutionFailed),
        Some(_) => Err(DocumentError::ResolutionFailed),
    }
}

#[cfg(test)]
#[path = "ported_tests.rs"]
mod ported_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const SIMILAR: &str = include_str!("fixtures/controlled_listenbrainz_similar_artists.json");
    const TRACKS: &str = include_str!("fixtures/controlled_listenbrainz_top_recordings.json");
    const POPULARITY: &str =
        include_str!("fixtures/controlled_listenbrainz_artist_popularity.json");

    #[test]
    fn similar_artists_keep_order_and_identifier_fields() {
        let payload: Value = serde_json::from_str(SIMILAR).unwrap();
        let items =
            parse_similar_artists(&payload, "cc197bad-dc9c-440d-a5b5-d52ba2e14234").unwrap();
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].name, payload[0]["name"].as_str().unwrap());
        assert_eq!(
            items[0].external_id.as_deref(),
            payload[0]["artist_mbid"].as_str()
        );
        assert_eq!(items[0].rank, Some(0));
    }

    #[test]
    fn similar_artists_enforce_reference_and_member_shapes() {
        assert_eq!(
            parse_similar_artists(
                &json!([{"artist_mbid":"x", "name":"x", "reference_mbid":"other"}]),
                "artist"
            ),
            Err(DocumentError::ResolutionFailed)
        );
        assert_eq!(
            parse_similar_artists(&json!([{"artist_mbid":"x", "name":7}]), "artist"),
            Err(DocumentError::ResolutionFailed)
        );
        assert_eq!(
            parse_similar_artists(&json!({}), "artist"),
            Err(DocumentError::ResolutionFailed)
        );
    }

    #[test]
    fn similar_list_is_limited_to_fifty_entries() {
        let items = (0..55)
            .map(
                |index| json!({"artist_mbid":format!("id{index}"),"name":format!("artist{index}")}),
            )
            .collect::<Vec<_>>();
        assert_eq!(
            parse_similar_artists(&Value::Array(items), "artist")
                .unwrap()
                .len(),
            50
        );
    }

    #[test]
    fn recordings_keep_counts_and_omit_missing_titles() {
        let payload: Value = serde_json::from_str(TRACKS).unwrap();
        let items = parse_recordings(payload.as_array().unwrap()).unwrap();
        assert!(!items.is_empty());
        assert!(items[0].rank.is_some());
        assert!(items[0].score.is_some());
        let missing: Value = serde_json::from_str(include_str!(
            "fixtures/controlled_listenbrainz_top_recordings_without_title.json"
        ))
        .unwrap();
        assert!(
            parse_recordings(missing.as_array().unwrap())
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn popularity_requires_matching_identity_and_preserves_nullable_counts() {
        let payload: Value = serde_json::from_str(POPULARITY).unwrap();
        let mbid = payload[0]["artist_mbid"].as_str().unwrap();
        assert!(parse_artist_popularity(&payload, mbid).unwrap().is_some());
        assert_eq!(parse_artist_popularity(&json!([]), mbid).unwrap(), None);
        assert_eq!(
            parse_artist_popularity(&json!([{"artist_mbid":"other"}]), mbid),
            Err(DocumentError::ResolutionFailed)
        );
        let nullable: Value = serde_json::from_str(include_str!(
            "fixtures/controlled_listenbrainz_artist_popularity_null_counts.json"
        ))
        .unwrap();
        assert_eq!(
            parse_artist_popularity(&nullable, nullable[0]["artist_mbid"].as_str().unwrap())
                .unwrap(),
            Some((None, None))
        );
    }
}
