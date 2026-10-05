use std::collections::BTreeSet;

use chrono::{DateTime, Utc};
use serde_json::{Map, Value};

use crate::features::{
    metadata::{
        domain::{Attribution, CandidateItem, FieldId, FieldValue, ImageFact, MatchBasis},
        licences::LicenceAssertion,
        refresh::{
            CandidateSource, DocumentCandidate, DocumentError, DocumentFieldError, DocumentOutcome,
        },
    },
    music_library::domain::SourceIdentity,
};

use super::{
    client::MusicBrainzClient,
    wikipedia::{self, DISAMBIGUATION_PAGE_TYPE},
};

const PAGE_TYPES: &[&str] = &[
    "official homepage",
    "wikipedia",
    "wikidata",
    "discogs",
    "last.fm",
];
const CHAIN_FIELDS: &[FieldId] = &[
    FieldId::BiographySummary,
    FieldId::BiographyContent,
    FieldId::BiographyLanguage,
    FieldId::ImagePortrait,
];
const BIOGRAPHY_FIELDS: &[FieldId] = &[
    FieldId::BiographySummary,
    FieldId::BiographyContent,
    FieldId::BiographyLanguage,
];
const MB_ATTRIBUTION: &str = "MusicBrainz";

pub struct MusicBrainzDocuments {
    client: std::sync::Arc<MusicBrainzClient>,
    provider_id: String,
}

impl MusicBrainzDocuments {
    pub fn new(config: &super::config::Config) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: std::sync::Arc::new(MusicBrainzClient::new(config)?),
            provider_id: "musicbrainz".to_owned(),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_client(client: std::sync::Arc<MusicBrainzClient>) -> Self {
        Self {
            client,
            provider_id: "musicbrainz".to_owned(),
        }
    }

    pub(crate) fn for_provider(&self, provider_id: &str) -> Self {
        Self {
            client: std::sync::Arc::clone(&self.client),
            provider_id: provider_id.to_owned(),
        }
    }

    pub(crate) fn provider_id(&self) -> &str {
        &self.provider_id
    }

    pub async fn fetch(
        &self,
        source: &SourceIdentity,
        language: &str,
        fields: &[FieldId],
    ) -> Result<DocumentOutcome, DocumentError> {
        let retrieved_at = Utc::now();
        let raw = self.client.fetch_artist(source.external_id()).await?;
        let artist = parse_artist(&raw, source.external_id())?;
        let relations = parse_relations(artist)?;
        let mut candidates = vec![identity_candidate(source)];
        candidates.extend(page_url_candidates(source, &relations));
        if let Some(tags) = tag_candidate(source, artist)? {
            candidates.push(tags);
        }
        let mut errors = Vec::new();
        if needs_chain(fields) {
            let (chain, chain_errors) = self.chain(&relations, language).await;
            candidates.extend(chain);
            errors.extend(chain_errors);
        }
        candidates.retain(|candidate| fields.contains(&candidate.field));
        errors.retain(|error| fields.contains(&error.field));
        Ok(outcome(source, retrieved_at, candidates, errors))
    }

    async fn chain(
        &self,
        relations: &[Relation],
        language: &str,
    ) -> (Vec<DocumentCandidate>, Vec<DocumentFieldError>) {
        let wiki_relation = wikipedia_relation_target(relations);
        let mut entity = None;
        let mut p18 = Vec::new();
        let mut candidates = Vec::new();
        let mut errors = Vec::new();
        if let Some(id) = wikidata_entity_id(relations) {
            match self
                .client
                .fetch_wikidata_entity(&id)
                .await
                .and_then(|value| parse_wikidata_entity(&value, &id))
            {
                Ok(value) => {
                    match p18_file_names(&value) {
                        Ok(names) => p18 = names,
                        Err(error) => errors.push(field_error(FieldId::ImagePortrait, &error)),
                    }
                    entity = Some(value);
                }
                Err(error) => {
                    errors.push(field_error(FieldId::ImagePortrait, &error));
                    if wiki_relation.is_none() {
                        errors.extend(
                            BIOGRAPHY_FIELDS
                                .iter()
                                .map(|field| field_error(*field, &error)),
                        );
                    }
                }
            }
        }
        let mut files = Vec::new();
        for name in commons_image_names(relations).into_iter().chain(p18) {
            if !files.contains(&name) {
                files.push(name);
            }
        }
        for (rank, name) in files.into_iter().enumerate() {
            match self
                .client
                .fetch_commons_imageinfo(&name)
                .await
                .and_then(|value| parse_commons_imageinfo(&value, &name))
            {
                Ok(Some(image)) => candidates.push(image_candidate(&image, rank as u32)),
                Ok(None) => errors.push(field_error(
                    FieldId::ImagePortrait,
                    &DocumentError::ResolutionFailed,
                )),
                Err(error) => errors.push(field_error(FieldId::ImagePortrait, &error)),
            }
        }
        let target = wiki_relation.or_else(|| {
            entity.as_ref().and_then(|value| {
                wikipedia::sitelink_wiki_target(
                    value.get("sitelinks").unwrap_or(&Value::Null),
                    Some(language),
                )
            })
        });
        if let Some((host, title)) = target {
            self.biography(&host, &title, &mut candidates, &mut errors)
                .await;
        }
        (candidates, errors)
    }

    async fn biography(
        &self,
        host: &str,
        requested_title: &str,
        candidates: &mut Vec<DocumentCandidate>,
        errors: &mut Vec<DocumentFieldError>,
    ) {
        let summary = match self
            .client
            .fetch_wikipedia_summary(host, requested_title)
            .await
            .and_then(|value| {
                wikipedia::parse_summary(&value).map_err(|_| DocumentError::ResolutionFailed)
            }) {
            Ok(summary) => Some(summary),
            Err(error) => {
                errors.push(field_error(FieldId::BiographySummary, &error));
                None
            }
        };
        if summary
            .as_ref()
            .and_then(|value| value.page_type.as_deref())
            == Some(DISAMBIGUATION_PAGE_TYPE)
        {
            return;
        }
        let extract = match self
            .client
            .fetch_wikipedia_extract(host, requested_title)
            .await
            .and_then(|value| {
                wikipedia::parse_extract(&value).map_err(|_| DocumentError::ResolutionFailed)
            }) {
            Ok(value) => value,
            Err(error) => {
                errors.push(field_error(FieldId::BiographyContent, &error));
                None
            }
        };
        let title = summary
            .as_ref()
            .and_then(|value| value.title.as_deref())
            .filter(|value| !value.trim().is_empty())
            .unwrap_or(requested_title);
        let page_url = summary
            .as_ref()
            .and_then(|value| value.page_url.clone())
            .unwrap_or_else(|| wikipedia_page_url(host, requested_title));
        let mut emitted = false;
        if let Some(text) = summary
            .as_ref()
            .and_then(|value| value.extract.as_deref())
            .filter(|value| !value.trim().is_empty())
        {
            if text.len() > 4096 {
                errors.push(field_too_large(FieldId::BiographySummary));
            } else {
                candidates.push(biography_candidate(
                    FieldId::BiographySummary,
                    title,
                    &page_url,
                    text,
                ));
                emitted = true;
            }
        }
        if let Some(text) = extract.as_deref().filter(|value| !value.trim().is_empty()) {
            if text.len() > 65_536 {
                errors.push(field_too_large(FieldId::BiographyContent));
            } else {
                candidates.push(biography_candidate(
                    FieldId::BiographyContent,
                    title,
                    &page_url,
                    text,
                ));
                emitted = true;
            }
        }
        if emitted && let Some(language) = wikipedia_host_language(host) {
            candidates.push(DocumentCandidate {
                field: FieldId::BiographyLanguage,
                source: wiki_source(title, &page_url),
                value: FieldValue::Text(language.to_owned()),
                language: Some(language.to_owned()),
                licence: wiki_licence(),
                attribution: Some(Attribution {
                    text: title.to_owned(),
                    url: Some(page_url.clone()),
                }),
                source_url: Some(page_url),
                rank: None,
                score: None,
                image: None,
                metric: None,
            });
        }
    }
}

#[derive(Debug)]
pub(crate) struct Relation {
    kind: String,
    resource: String,
}
#[derive(Debug)]
struct CommonsImage {
    name: String,
    page: String,
    image: String,
    licence: String,
    licence_url: Option<String>,
    attribution: Option<String>,
    required: bool,
    share_alike: bool,
}

fn outcome(
    source: &SourceIdentity,
    retrieved_at: DateTime<Utc>,
    candidates: Vec<DocumentCandidate>,
    errors: Vec<DocumentFieldError>,
) -> DocumentOutcome {
    DocumentOutcome {
        address: source.clone(),
        source_revision: None,
        retrieved_at,
        candidates,
        errors,
    }
}

pub(crate) fn parse_artist<'a>(
    payload: &'a Value,
    mbid: &str,
) -> Result<&'a Map<String, Value>, DocumentError> {
    let artist = payload.as_object().ok_or(DocumentError::ResolutionFailed)?;
    let id = artist
        .get("id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .ok_or(DocumentError::ResolutionFailed)?;
    if id != mbid {
        return Err(DocumentError::ResolutionFailed);
    }
    Ok(artist)
}

pub(crate) fn parse_relations(artist: &Map<String, Value>) -> Result<Vec<Relation>, DocumentError> {
    let Some(value) = artist.get("relations") else {
        return Ok(Vec::new());
    };
    let list = value.as_array().ok_or(DocumentError::ResolutionFailed)?;
    list.iter()
        .map(|entry| {
            let entry = entry.as_object().ok_or(DocumentError::ResolutionFailed)?;
            let kind = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned();
            let resource = entry
                .get("url")
                .and_then(|url| url.get("resource"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|resource| !resource.is_empty())
                .unwrap_or_default()
                .to_owned();
            Ok(Relation { kind, resource })
        })
        .collect()
}

fn identity_candidate(source: &SourceIdentity) -> DocumentCandidate {
    DocumentCandidate {
        field: FieldId::IdentityMbid,
        source: mb_source(
            source,
            musicbrainz_artist_page(source.external_id()),
            MatchBasis::Identifier,
        ),
        value: FieldValue::Text(source.external_id().to_owned()),
        language: None,
        licence: mb_core_licence(),
        attribution: None,
        source_url: None,
        rank: None,
        score: None,
        image: None,
        metric: None,
    }
}

fn page_url_candidates(source: &SourceIdentity, relations: &[Relation]) -> Vec<DocumentCandidate> {
    let mut seen = BTreeSet::new();
    relations
        .iter()
        .filter(|relation| {
            PAGE_TYPES.contains(&relation.kind.as_str())
                && !relation.resource.is_empty()
                && seen.insert(relation.resource.clone())
        })
        .enumerate()
        .map(|(rank, relation)| DocumentCandidate {
            field: FieldId::PageUrl,
            source: mb_source(
                source,
                relation.resource.clone(),
                MatchBasis::EstablishedSource,
            ),
            value: FieldValue::Reference(relation.resource.clone()),
            language: None,
            licence: mb_core_licence(),
            attribution: None,
            source_url: Some(relation.resource.clone()),
            rank: Some(rank as u32),
            score: None,
            image: None,
            metric: None,
        })
        .collect()
}

fn tag_candidate(
    source: &SourceIdentity,
    artist: &Map<String, Value>,
) -> Result<Option<DocumentCandidate>, DocumentError> {
    let mut names = Vec::new();
    for member in ["tags", "genres"] {
        let Some(value) = artist.get(member) else {
            continue;
        };
        let values = value.as_array().ok_or(DocumentError::ResolutionFailed)?;
        for entry in values {
            let name = entry
                .as_object()
                .ok_or(DocumentError::ResolutionFailed)?
                .get("name")
                .and_then(Value::as_str)
                .ok_or(DocumentError::ResolutionFailed)?
                .trim();
            if !name.is_empty() && name.len() <= 128 {
                names.push(name.to_owned());
            }
        }
    }
    names.truncate(32);
    if names.is_empty() {
        return Ok(None);
    }
    let source_info = mb_source(
        source,
        musicbrainz_artist_page(source.external_id()),
        MatchBasis::Identifier,
    );
    let items = names
        .into_iter()
        .enumerate()
        .map(|(rank, name)| CandidateItem {
            name,
            external_id: Some(source.external_id().to_owned()),
            url: None,
            rank: Some(rank as u32),
            score: None,
            artists: Vec::new(),
            release_name: None,
            duration_ms: None,
            isrc: None,
            recording_id: None,
            ranking_scope: None,
        })
        .collect();
    Ok(Some(DocumentCandidate {
        field: FieldId::Tag,
        source: source_info,
        value: FieldValue::Candidates(items),
        language: None,
        licence: mb_supplementary_licence(),
        attribution: Some(Attribution {
            text: MB_ATTRIBUTION.to_owned(),
            url: Some("https://musicbrainz.org".to_owned()),
        }),
        source_url: None,
        rank: None,
        score: None,
        image: None,
        metric: None,
    }))
}

fn wikidata_entity_id(relations: &[Relation]) -> Option<String> {
    relations
        .iter()
        .filter(|relation| relation.kind == "wikidata")
        .find_map(|relation| {
            let url = reqwest::Url::parse(&relation.resource).ok()?;
            if !matches!(url.host_str()?, "www.wikidata.org" | "wikidata.org") {
                return None;
            }
            let id = url.path().strip_prefix("/wiki/")?;
            (id.strip_prefix('Q')?.chars().all(|ch| ch.is_ascii_digit()) && id.len() > 1)
                .then(|| id.to_owned())
        })
}

fn wikipedia_relation_target(relations: &[Relation]) -> Option<(String, String)> {
    relations
        .iter()
        .filter(|relation| relation.kind == "wikipedia")
        .find_map(|relation| wikipedia::wikipedia_target(&relation.resource))
}
fn commons_image_names(relations: &[Relation]) -> Vec<String> {
    relations
        .iter()
        .filter(|relation| relation.kind == "image")
        .filter_map(|relation| commons_file_name_from_page(&relation.resource))
        .collect()
}

fn parse_wikidata_entity(payload: &Value, id: &str) -> Result<Map<String, Value>, DocumentError> {
    let entities = payload
        .get("entities")
        .and_then(Value::as_object)
        .ok_or(DocumentError::ResolutionFailed)?;
    match entities.get(id) {
        None | Some(Value::Null) => Ok(Map::new()),
        Some(entity) => entity
            .as_object()
            .cloned()
            .ok_or(DocumentError::ResolutionFailed),
    }
}

fn p18_file_names(entity: &Map<String, Value>) -> Result<Vec<String>, DocumentError> {
    let Some(claims) = entity.get("claims") else {
        return Ok(Vec::new());
    };
    let claims = claims.as_object().ok_or(DocumentError::ResolutionFailed)?;
    let Some(items) = claims.get("P18") else {
        return Ok(Vec::new());
    };
    let items = items.as_array().ok_or(DocumentError::ResolutionFailed)?;
    let mut names = Vec::new();
    for item in items {
        let item = item.as_object().ok_or(DocumentError::ResolutionFailed)?;
        let Some(value) = item
            .get("mainsnak")
            .and_then(|value| value.get("datavalue"))
            .and_then(|value| value.get("value"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if let Some(name) = commons_file_name(value) {
            names.push(name);
        }
    }
    Ok(names)
}

fn parse_commons_imageinfo(
    payload: &Value,
    name: &str,
) -> Result<Option<CommonsImage>, DocumentError> {
    let payload = payload.as_object().ok_or(DocumentError::ResolutionFailed)?;
    let Some(query) = payload.get("query") else {
        return Ok(None);
    };
    let query = query.as_object().ok_or(DocumentError::ResolutionFailed)?;
    let Some(pages) = query.get("pages") else {
        return Ok(None);
    };
    let pages = pages.as_object().ok_or(DocumentError::ResolutionFailed)?;
    for page in pages.values() {
        let page = page.as_object().ok_or(DocumentError::ResolutionFailed)?;
        if let Some(title) = page.get("title").and_then(Value::as_str)
            && commons_file_name(title).as_deref() != Some(name)
        {
            continue;
        }
        if page.contains_key("missing") {
            return Err(DocumentError::ResolutionFailed);
        }
        let Some(entries) = page.get("imageinfo") else {
            continue;
        };
        let entries = entries.as_array().ok_or(DocumentError::ResolutionFailed)?;
        let Some(info) = entries.first() else {
            continue;
        };
        let info = info.as_object().ok_or(DocumentError::ResolutionFailed)?;
        let Some(metadata) = info.get("extmetadata").and_then(Value::as_object) else {
            continue;
        };
        let text = |key: &str| {
            metadata
                .get(key)
                .and_then(|value| value.get("value").or(Some(value)))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        };
        let Some(licence) = text("LicenseShortName") else {
            continue;
        };
        if format!("commons:{licence}").len() > 200 {
            continue;
        }
        let attribution = ["Artist", "Credit"].into_iter().find_map(|key| {
            text(key)
                .map(|value| strip_markup(&value))
                .filter(|value| !value.is_empty() && value.len() <= 1000)
        });
        let required = metadata
            .get("AttributionRequired")
            .and_then(|v| v.get("value").or(Some(v)))
            .map(|v| match v {
                Value::Bool(value) => *value,
                Value::String(value) => matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "true" | "yes" | "1"
                ),
                _ => true,
            })
            .unwrap_or(true);
        let page_url =
            http_url(info.get("descriptionurl")).unwrap_or_else(|| commons_file_page(name));
        let image = http_url(info.get("url")).unwrap_or_else(|| page_url.clone());
        let share = licence
            .to_ascii_lowercase()
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|token| token == "sa");
        return Ok(Some(CommonsImage {
            name: name.to_owned(),
            page: page_url,
            image,
            licence,
            licence_url: text("LicenseUrl"),
            attribution,
            required,
            share_alike: share,
        }));
    }
    Ok(None)
}

fn image_candidate(image: &CommonsImage, rank: u32) -> DocumentCandidate {
    let licence_url = image
        .licence_url
        .clone()
        .unwrap_or_else(|| image.page.clone());
    DocumentCandidate {
        field: FieldId::ImagePortrait,
        source: CandidateSource {
            provider_id: "commons".to_owned(),
            external_id: image.name.clone(),
            url: Some(image.page.clone()),
            match_basis: MatchBasis::EstablishedSource,
        },
        value: FieldValue::Reference(image.image.clone()),
        language: None,
        licence: LicenceAssertion {
            id: format!("commons:{}", image.licence),
            url: Some(licence_url),
            attribution_required: image.required,
            redistribution_allowed: true,
            cache_allowed: true,
            share_alike: image.share_alike,
            non_commercial: false,
            storage_cap_bytes: None,
        },
        attribution: image.attribution.as_ref().map(|text| Attribution {
            text: text.clone(),
            url: None,
        }),
        source_url: None,
        rank: Some(rank),
        score: None,
        image: Some(ImageFact {
            role: "portrait".to_owned(),
            width: None,
            height: None,
            content_type: "application/octet-stream".to_owned(),
        }),
        metric: None,
    }
}

fn biography_candidate(field: FieldId, title: &str, url: &str, text: &str) -> DocumentCandidate {
    DocumentCandidate {
        field,
        source: wiki_source(title, url),
        value: FieldValue::Text(text.to_owned()),
        language: None,
        licence: wiki_licence(),
        attribution: Some(Attribution {
            text: title.to_owned(),
            url: Some(url.to_owned()),
        }),
        source_url: Some(url.to_owned()),
        rank: None,
        score: None,
        image: None,
        metric: None,
    }
}
fn wiki_source(title: &str, url: &str) -> CandidateSource {
    CandidateSource {
        provider_id: "wikipedia".to_owned(),
        external_id: title.to_owned(),
        url: Some(url.to_owned()),
        match_basis: MatchBasis::EstablishedSource,
    }
}
fn mb_source(source: &SourceIdentity, url: String, basis: MatchBasis) -> CandidateSource {
    CandidateSource {
        provider_id: "musicbrainz".to_owned(),
        external_id: source.external_id().to_owned(),
        url: Some(url),
        match_basis: basis,
    }
}
fn musicbrainz_artist_page(mbid: &str) -> String {
    format!("https://musicbrainz.org/artist/{}", encode_path(mbid))
}
fn wikipedia_page_url(host: &str, title: &str) -> String {
    format!(
        "https://{host}/wiki/{}",
        encode_path(&title.replace(' ', "_"))
    )
}
fn wikipedia_host_language(host: &str) -> Option<&str> {
    let language = host.strip_suffix(".wikipedia.org")?;
    (language.len() >= 2).then_some(language.split('.').next()?)
}
fn commons_file_page(name: &str) -> String {
    format!(
        "https://commons.wikimedia.org/wiki/File:{}",
        encode_path(&name.replace(' ', "_"))
    )
}
fn encode_path(value: &str) -> String {
    let mut result = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            result.push(byte as char);
        } else {
            result.push_str(&format!("%{byte:02X}"));
        }
    }
    result
}
fn commons_file_name_from_page(raw: &str) -> Option<String> {
    let url = reqwest::Url::parse(raw).ok()?;
    if url.host_str()? != "commons.wikimedia.org" {
        return None;
    }
    commons_file_name(url.path().strip_prefix("/wiki/")?)
}
fn commons_file_name(raw: &str) -> Option<String> {
    let mut name = raw.trim().replace('_', " ");
    if name.to_ascii_lowercase().starts_with("file:") {
        name = name[5..].trim().to_owned();
    }
    (!name.is_empty()).then_some(name)
}
fn strip_markup(text: &str) -> String {
    let mut out = String::new();
    let mut inside = false;
    for ch in text.chars() {
        match ch {
            '<' => inside = true,
            '>' => inside = false,
            _ if !inside => out.push(ch),
            _ => {}
        }
    }
    out.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn http_url(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|url| url.starts_with("http://") || url.starts_with("https://"))
        .map(str::to_owned)
}
fn field_error(field: FieldId, error: &DocumentError) -> DocumentFieldError {
    crate::integrations::document_http::field_error(field, error)
}
fn field_too_large(field: FieldId) -> DocumentFieldError {
    DocumentFieldError {
        field,
        code: "resolution_failed".to_owned(),
        retryable: false,
    }
}
fn mb_core_licence() -> LicenceAssertion {
    LicenceAssertion {
        id: "musicbrainz".to_owned(),
        url: Some("https://creativecommons.org/publicdomain/zero/1.0/".to_owned()),
        attribution_required: false,
        redistribution_allowed: true,
        cache_allowed: true,
        share_alike: false,
        non_commercial: false,
        storage_cap_bytes: None,
    }
}
fn mb_supplementary_licence() -> LicenceAssertion {
    LicenceAssertion {
        id: "musicbrainz.supplementary".to_owned(),
        url: Some("https://creativecommons.org/licenses/by-nc-sa/3.0/".to_owned()),
        attribution_required: true,
        redistribution_allowed: true,
        cache_allowed: true,
        share_alike: true,
        non_commercial: true,
        storage_cap_bytes: None,
    }
}
fn wiki_licence() -> LicenceAssertion {
    LicenceAssertion {
        id: "wikipedia".to_owned(),
        url: Some("https://creativecommons.org/licenses/by-sa/4.0/".to_owned()),
        attribution_required: true,
        redistribution_allowed: true,
        cache_allowed: true,
        share_alike: true,
        non_commercial: false,
        storage_cap_bytes: None,
    }
}
fn needs_chain(fields: &[FieldId]) -> bool {
    fields.iter().any(|field| CHAIN_FIELDS.contains(field))
}

#[cfg(test)]
#[path = "tests.rs"]
mod ported_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ARTIST: &str = include_str!("fixtures/controlled_musicbrainz_artist_document.json");
    const WIKIDATA: &str = include_str!("fixtures/controlled_wikidata_entity.json");
    const COMMONS: &str = include_str!("fixtures/controlled_commons_imageinfo.json");

    #[test]
    fn artist_and_relation_parsers_reject_wrong_shapes() {
        let payload: Value = serde_json::from_str(ARTIST).unwrap();
        let artist = parse_artist(&payload, "cc197bad-dc9c-440d-a5b5-d52ba2e14234").unwrap();
        assert!(!parse_relations(artist).unwrap().is_empty());
        assert_eq!(
            parse_artist(&json!([]), "id").unwrap_err(),
            DocumentError::ResolutionFailed
        );
        assert_eq!(
            parse_artist(&json!({"id":"other"}), "id").unwrap_err(),
            DocumentError::ResolutionFailed
        );
        assert_eq!(
            parse_relations(json!({"relations":false}).as_object().unwrap()).unwrap_err(),
            DocumentError::ResolutionFailed
        );
    }

    #[test]
    fn wikidata_and_commons_chain_parsers_preserve_file_order_and_licence() {
        let value: Value = serde_json::from_str(WIKIDATA).unwrap();
        let entity = parse_wikidata_entity(&value, "Q11647").unwrap();
        assert!(!p18_file_names(&entity).unwrap().is_empty());
        let value: Value = serde_json::from_str(COMMONS).unwrap();
        let image = parse_commons_imageinfo(&value, "ColdplayWembley.jpg")
            .unwrap()
            .unwrap();
        let candidate = image_candidate(&image, 0);
        assert_eq!(candidate.source.provider_id, "commons");
        assert_eq!(candidate.field, FieldId::ImagePortrait);
        assert!(candidate.licence.id.starts_with("commons:"));
    }

    #[test]
    fn a_wrong_typed_p18_claim_is_a_parse_failure() {
        let entity =
            parse_wikidata_entity(&json!({"entities":{"Q1":{"claims":{"P18":[false]}}}}), "Q1")
                .unwrap();
        assert_eq!(
            p18_file_names(&entity),
            Err(DocumentError::ResolutionFailed)
        );
    }

    #[test]
    fn a_missing_commons_file_is_a_parse_failure() {
        let missing: Value = serde_json::from_str(include_str!(
            "fixtures/controlled_commons_imageinfo_missing.json"
        ))
        .unwrap();
        assert!(matches!(
            parse_commons_imageinfo(&missing, "ColdplayWembley.jpg"),
            Err(DocumentError::ResolutionFailed)
        ));
    }

    #[test]
    fn tags_keep_at_most_thirty_two_entries_and_one_hundred_twenty_eight_bytes() {
        let values = (0..40)
            .map(|index| json!({"name": if index == 0 { "x".repeat(129) } else { format!("tag-{index}") }}))
            .collect::<Vec<_>>();
        let artist = json!({"tags":values});
        let source = SourceIdentity::new(
            crate::features::music_library::domain::ProviderId::new("musicbrainz".to_owned())
                .unwrap(),
            crate::features::music_library::domain::SourceScope::Global,
            "mbid".to_owned(),
        )
        .unwrap();
        let candidate = tag_candidate(&source, artist.as_object().unwrap())
            .unwrap()
            .unwrap();
        let FieldValue::Candidates(values) = candidate.value else {
            panic!("the tag field contains candidate values")
        };
        assert_eq!(values.len(), 32);
        assert!(values.iter().all(|value| value.name.len() <= 128));
    }

    #[test]
    fn field_needs_are_restricted_to_chain_fields() {
        assert!(!needs_chain(&[
            FieldId::IdentityMbid,
            FieldId::PageUrl,
            FieldId::Tag
        ]));
        assert!(needs_chain(&[FieldId::ImagePortrait]));
    }

    #[test]
    fn wikipedia_target_and_sitelinks_choose_the_requested_host() {
        assert_eq!(
            wikipedia::wikipedia_target("https://de.wikipedia.org/wiki/Coldplay"),
            Some(("de.wikipedia.org".to_owned(), "Coldplay".to_owned()))
        );
        assert_eq!(
            wikipedia::wikipedia_target("https://example.org/wiki/Coldplay"),
            None
        );
        assert_eq!(
            wikipedia::sitelink_wiki_target(
                &json!({"dewiki":{"title":"Coldplay"},"enwiki":{"title":"Coldplay"}}),
                Some("de")
            ),
            Some(("de.wikipedia.org".to_owned(), "Coldplay".to_owned()))
        );
    }

    #[tokio::test]
    async fn one_document_follows_the_musicbrainz_wikimedia_and_wikipedia_chain() {
        use axum::{
            Router,
            extract::State,
            http::{StatusCode, Uri},
            routing::any,
        };
        use std::sync::{Arc, Mutex};
        use tokio::net::TcpListener;

        async fn response(
            State(requests): State<Arc<Mutex<Vec<String>>>>,
            uri: Uri,
        ) -> (StatusCode, String) {
            let target = uri.to_string();
            requests.lock().unwrap().push(target.clone());
            let body = if target.starts_with("/ws/2/artist/") {
                ARTIST
            } else if target.contains("/api/rest_v1/page/summary/") {
                include_str!("fixtures/controlled_wikipedia_summary.json")
            } else if target.contains("prop=imageinfo") {
                COMMONS
            } else if target.contains("action=wbgetentities") {
                WIKIDATA
            } else if target.contains("prop=extracts") {
                include_str!("fixtures/controlled_wikipedia_extract.json")
            } else {
                "{}"
            };
            (StatusCode::OK, body.to_owned())
        }

        let requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .fallback(any(response))
            .with_state(Arc::clone(&requests));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let host = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let base = reqwest::Url::parse(&format!("http://{host}/")).unwrap();
        let client = MusicBrainzClient::with_hosts(
            base.clone(),
            base.clone(),
            base.clone(),
            super::super::client::WikipediaHosts::Fixed(base),
            "mb-agent".to_owned(),
            "wiki-agent".to_owned(),
            Arc::new(crate::platform::pacing::CallLimiter::new(
                std::time::Duration::ZERO,
            )),
        )
        .unwrap();
        let docs = MusicBrainzDocuments::with_client(Arc::new(client));
        let source = SourceIdentity::new(
            crate::features::music_library::domain::ProviderId::new("musicbrainz".to_owned())
                .unwrap(),
            crate::features::music_library::domain::SourceScope::Global,
            "cc197bad-dc9c-440d-a5b5-d52ba2e14234".to_owned(),
        )
        .unwrap();
        let fields = [
            FieldId::IdentityMbid,
            FieldId::PageUrl,
            FieldId::Tag,
            FieldId::BiographySummary,
            FieldId::BiographyContent,
            FieldId::BiographyLanguage,
            FieldId::ImagePortrait,
        ];
        let document = docs.fetch(&source, "en", &fields).await.unwrap();
        assert_eq!(document.address, source);
        assert!(document.errors.is_empty());
        assert!(
            document
                .candidates
                .iter()
                .any(|candidate| candidate.field == FieldId::ImagePortrait)
        );
        assert!(
            document
                .candidates
                .iter()
                .any(|candidate| candidate.field == FieldId::BiographySummary)
        );
        assert_eq!(requests.lock().unwrap().len(), 5);
    }
}
