use chrono::Utc;
use serde_json::Value;

use crate::features::{
    metadata::{
        domain::{Attribution, FieldId, FieldValue, ImageFact, MatchBasis},
        licences::LicenceAssertion,
        refresh::{
            CandidateSource, DocumentCandidate, DocumentError, DocumentFieldError, DocumentOutcome,
        },
    },
    music_library::domain::SourceIdentity,
};

const PROVIDER: &str = "theaudiodb";
const TERMS: &str = "https://www.theaudiodb.com/docs_terms_of_use.php";
const ATTRIBUTION: &str = "Data provided by TheAudioDB";
const MBID: &str = "strMusicBrainzID";
const BIOGRAPHY: &[&str] = &[
    "strBiographyEN",
    "strBiography",
    "strBiographyDE",
    "strBiographyFR",
    "strBiographyCN",
    "strBiographyIT",
    "strBiographyJP",
    "strBiographyRU",
    "strBiographyES",
    "strBiographyPT",
    "strBiographySE",
    "strBiographyNL",
    "strBiographyHU",
    "strBiographyNO",
    "strBiographyIL",
    "strBiographyPL",
];
const PORTRAIT: &[&str] = &["strArtistThumb"];
const WIDE: &[&str] = &[
    "strArtistWideThumb",
    "strArtistFanart",
    "strArtistFanart2",
    "strArtistFanart3",
    "strArtistFanart4",
];

pub(crate) fn parse_artist(
    payload: &Value,
) -> Result<&serde_json::Map<String, Value>, DocumentError> {
    let artists = payload
        .as_object()
        .and_then(|object| object.get("artists"))
        .ok_or(DocumentError::ResolutionFailed)?;
    if artists.is_null() {
        return Err(DocumentError::NotFound);
    }
    let artists = artists.as_array().ok_or(DocumentError::ResolutionFailed)?;
    let Some(artist) = artists.first() else {
        return Err(DocumentError::NotFound);
    };
    artist.as_object().ok_or(DocumentError::ResolutionFailed)
}

pub(crate) fn document(
    source: &SourceIdentity,
    artist: &serde_json::Map<String, Value>,
    language: &str,
    fields: &[FieldId],
    retrieved_at: chrono::DateTime<Utc>,
) -> Result<DocumentOutcome, DocumentError> {
    let external_id = artist
        .get(MBID)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or(DocumentError::ResolutionFailed)?
        .to_owned();
    let id_url = artist
        .get("idArtist")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|id| format!("https://www.theaudiodb.com/artist/{id}"));
    let producing = CandidateSource {
        provider_id: PROVIDER.to_owned(),
        external_id,
        url: id_url.clone(),
        match_basis: MatchBasis::EstablishedSource,
    };
    let creative_commons = is_creative_commons(artist.get("strCreativeCommons"));
    let terms = licence(false);
    let artwork_terms = licence(creative_commons);
    let attribution = Some(Attribution {
        text: ATTRIBUTION.to_owned(),
        url: id_url,
    });
    let mut candidates = Vec::new();
    let mut errors = Vec::new();

    if let Some((member, text)) = select_biography(artist, language) {
        if text.len() > 65_536 {
            errors.push(field_error(FieldId::BiographyContent));
        } else {
            candidates.push(candidate(
                FieldId::BiographyContent,
                &producing,
                FieldValue::Text(text),
                terms.clone(),
                attribution.clone(),
                None,
                None,
            ));
        }
        if let Some(declared) = biography_language(member) {
            let mut language_candidate = candidate(
                FieldId::BiographyLanguage,
                &producing,
                FieldValue::Text(declared.to_owned()),
                terms.clone(),
                attribution.clone(),
                None,
                None,
            );
            language_candidate.language = Some(declared.to_owned());
            candidates.push(language_candidate);
        }
    }
    for (field, members, role) in [
        (FieldId::ImagePortrait, PORTRAIT, "portrait"),
        (FieldId::ImageWide, WIDE, "wide"),
    ] {
        match parse_image_field(artist, members) {
            Ok(Some(reference)) => candidates.push(candidate(
                field,
                &producing,
                FieldValue::Reference(reference),
                artwork_terms.clone(),
                attribution.clone(),
                Some(ImageFact {
                    role: role.to_owned(),
                    width: None,
                    height: None,
                    content_type: "application/octet-stream".to_owned(),
                }),
                None,
            )),
            Ok(None) => {}
            Err(()) => errors.push(field_error(field)),
        }
    }
    candidates.retain(|candidate| fields.contains(&candidate.field));
    errors.retain(|error| fields.contains(&error.field));
    Ok(DocumentOutcome {
        address: source.clone(),
        source_revision: None,
        retrieved_at,
        candidates,
        errors,
    })
}

fn candidate(
    field: FieldId,
    source: &CandidateSource,
    value: FieldValue,
    licence: LicenceAssertion,
    attribution: Option<Attribution>,
    image: Option<ImageFact>,
    rank: Option<u32>,
) -> DocumentCandidate {
    DocumentCandidate {
        field,
        source: source.clone(),
        value,
        language: None,
        licence,
        attribution,
        source_url: None,
        rank,
        score: None,
        image,
        metric: None,
    }
}

pub(crate) fn select_biography(
    artist: &serde_json::Map<String, Value>,
    language: &str,
) -> Option<(&'static str, String)> {
    let first = match language {
        "de" => Some("strBiographyDE"),
        "fr" => Some("strBiographyFR"),
        "zh" => Some("strBiographyCN"),
        "it" => Some("strBiographyIT"),
        "ja" => Some("strBiographyJP"),
        "ru" => Some("strBiographyRU"),
        "es" => Some("strBiographyES"),
        "pt" => Some("strBiographyPT"),
        "sv" => Some("strBiographySE"),
        "nl" => Some("strBiographyNL"),
        "hu" => Some("strBiographyHU"),
        "no" => Some("strBiographyNO"),
        "he" => Some("strBiographyIL"),
        "pl" => Some("strBiographyPL"),
        _ => None,
    };
    let ordered = first.into_iter().chain(BIOGRAPHY.iter().copied());
    for member in ordered {
        if let Some(text) = artist
            .get(member)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return Some((member, text.to_owned()));
        }
    }
    None
}

fn biography_language(member: &str) -> Option<&'static str> {
    match member {
        "strBiographyEN" => Some("en"),
        "strBiographyDE" => Some("de"),
        "strBiographyFR" => Some("fr"),
        "strBiographyCN" => Some("zh"),
        "strBiographyIT" => Some("it"),
        "strBiographyJP" => Some("ja"),
        "strBiographyRU" => Some("ru"),
        "strBiographyES" => Some("es"),
        "strBiographyPT" => Some("pt"),
        "strBiographySE" => Some("sv"),
        "strBiographyNL" => Some("nl"),
        "strBiographyHU" => Some("hu"),
        "strBiographyNO" => Some("no"),
        "strBiographyIL" => Some("he"),
        "strBiographyPL" => Some("pl"),
        _ => None,
    }
}

fn parse_image_field(
    artist: &serde_json::Map<String, Value>,
    members: &[&str],
) -> Result<Option<String>, ()> {
    for member in members {
        match artist.get(*member) {
            None | Some(Value::Null) => continue,
            Some(Value::String(value))
                if value.trim().starts_with("http://") || value.trim().starts_with("https://") =>
            {
                return Ok(Some(value.trim().to_owned()));
            }
            Some(Value::String(_)) => continue,
            Some(_) => return Err(()),
        }
    }
    Ok(None)
}

fn is_creative_commons(value: Option<&Value>) -> bool {
    match value {
        Some(Value::Bool(true)) => true,
        Some(Value::String(value)) => {
            matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "yes")
        }
        _ => false,
    }
}

fn field_error(field: FieldId) -> DocumentFieldError {
    DocumentFieldError {
        field,
        code: "resolution_failed".to_owned(),
        retryable: false,
    }
}

fn licence(creative_commons: bool) -> LicenceAssertion {
    LicenceAssertion {
        id: if creative_commons {
            "commons:CC BY 4.0"
        } else {
            "theaudiodb"
        }
        .to_owned(),
        url: Some(
            if creative_commons {
                "https://creativecommons.org/licenses/by/4.0/"
            } else {
                TERMS
            }
            .to_owned(),
        ),
        attribution_required: true,
        redistribution_allowed: creative_commons,
        cache_allowed: true,
        share_alike: false,
        non_commercial: false,
        storage_cap_bytes: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const FIXTURE: &str = include_str!("fixtures/audiodb_artist_document.json");

    #[test]
    fn parse_artist_and_document_fixture() {
        let payload: Value = serde_json::from_str(FIXTURE).unwrap();
        let artist = parse_artist(&payload).unwrap();
        let source = source();
        let result = document(
            &source,
            artist,
            "en",
            &[
                FieldId::BiographyContent,
                FieldId::ImagePortrait,
                FieldId::ImageWide,
            ],
            Utc::now(),
        )
        .unwrap();
        assert_eq!(result.candidates.len(), 3);
        assert_eq!(result.address, source);
        assert!(result.errors.is_empty());
    }

    #[test]
    fn collection_absence_and_malformed_shapes_are_distinct() {
        assert_eq!(
            parse_artist(&json!({"artists":null})).unwrap_err(),
            DocumentError::NotFound
        );
        assert_eq!(
            parse_artist(&json!({"artists":[]})).unwrap_err(),
            DocumentError::NotFound
        );
        assert_eq!(
            parse_artist(&json!({})).unwrap_err(),
            DocumentError::ResolutionFailed
        );
        assert_eq!(
            parse_artist(&json!({"artists":{}})).unwrap_err(),
            DocumentError::ResolutionFailed
        );
        assert_eq!(
            parse_artist(&json!({"artists":[7]})).unwrap_err(),
            DocumentError::ResolutionFailed
        );
    }

    fn source() -> SourceIdentity {
        SourceIdentity::new(
            crate::features::music_library::domain::ProviderId::new(PROVIDER.to_owned()).unwrap(),
            crate::features::music_library::domain::SourceScope::Global,
            "mbid".to_owned(),
        )
        .unwrap()
    }
}
