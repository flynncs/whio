use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenceTerms {
    pub id: String,
    pub revision: u32,
    pub name: String,
    pub url: Option<String>,
    pub attribution_required: bool,
    pub redistribution_allowed: bool,
    pub cache_allowed: bool,
    pub share_alike: bool,
    pub non_commercial: bool,
    pub storage_cap_bytes: Option<u64>,
    pub terms_basis: Option<String>,
    pub uncertainty: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LicenceAssertion {
    pub id: String,
    pub url: Option<String>,
    pub attribution_required: bool,
    pub redistribution_allowed: bool,
    pub cache_allowed: bool,
    pub share_alike: bool,
    pub non_commercial: bool,
    pub storage_cap_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UntrustedLicence {
    #[error("licence `{id}` is not in the registry")]
    Unknown { id: String },
    #[error("licence `{id}` disagrees with the registry on `{member}`")]
    Disagreement { id: String, member: &'static str },
}

const COMMONS_ID: &str = "commons";
const COMMONS_PREFIX: &str = "commons:";
const COMMONS_REVISION: u32 = 0;
const REGISTRY_REVISION: u32 = 1;

pub fn registry() -> &'static [LicenceTerms] {
    static REGISTRY: OnceLock<Vec<LicenceTerms>> = OnceLock::new();

    REGISTRY.get_or_init(builtin).as_slice()
}

pub fn licence_terms(id: &str, revision: u32) -> Option<LicenceTerms> {
    if let Some(short_name) = commons_short_name(id) {
        return commons_terms(id, short_name, revision);
    }

    registry()
        .iter()
        .find(|terms| terms.id == id && terms.revision == revision)
        .cloned()
}

pub fn resolve(
    assertion: &LicenceAssertion,
    registry: &[LicenceTerms],
) -> Result<LicenceTerms, UntrustedLicence> {
    let Some(base) = registry.iter().find(|terms| terms.id == COMMONS_ID) else {
        return Err(UntrustedLicence::Unknown {
            id: assertion.id.clone(),
        });
    };

    if let Some(short_name) = commons_short_name(&assertion.id) {
        if !assertion.redistribution_allowed {
            return Err(disagreement(&assertion.id, "redistribution_allowed"));
        }
        if assertion.cache_allowed != base.cache_allowed {
            return Err(disagreement(&assertion.id, "cache_allowed"));
        }
        if assertion.non_commercial != base.non_commercial {
            return Err(disagreement(&assertion.id, "non_commercial"));
        }
        if assertion.storage_cap_bytes != base.storage_cap_bytes {
            return Err(disagreement(&assertion.id, "storage_cap_bytes"));
        }

        return Ok(LicenceTerms {
            id: assertion.id.clone(),
            revision: COMMONS_REVISION,
            name: short_name.to_owned(),
            url: assertion.url.clone(),
            attribution_required: assertion.attribution_required,
            redistribution_allowed: true,
            cache_allowed: base.cache_allowed,
            share_alike: assertion.share_alike,
            non_commercial: base.non_commercial,
            storage_cap_bytes: base.storage_cap_bytes,
            terms_basis: Some(format!(
                "{COMMONS_ID} {short_name}: per-file licence from extmetadata"
            )),
            uncertainty: false,
        });
    }

    let terms = registry
        .iter()
        .find(|terms| terms.id == assertion.id)
        .ok_or_else(|| UntrustedLicence::Unknown {
            id: assertion.id.clone(),
        })?;

    for (member, asserted, registered) in [
        (
            "attribution_required",
            assertion.attribution_required,
            terms.attribution_required,
        ),
        (
            "redistribution_allowed",
            assertion.redistribution_allowed,
            terms.redistribution_allowed,
        ),
        (
            "cache_allowed",
            assertion.cache_allowed,
            terms.cache_allowed,
        ),
        ("share_alike", assertion.share_alike, terms.share_alike),
        (
            "non_commercial",
            assertion.non_commercial,
            terms.non_commercial,
        ),
    ] {
        if asserted != registered {
            return Err(disagreement(&assertion.id, member));
        }
    }

    if assertion.storage_cap_bytes != terms.storage_cap_bytes {
        return Err(disagreement(&assertion.id, "storage_cap_bytes"));
    }

    Ok(terms.clone())
}

pub fn commons_short_name(id: &str) -> Option<&str> {
    id.strip_prefix(COMMONS_PREFIX)
        .filter(|short_name| !short_name.is_empty())
}

fn commons_terms(id: &str, short_name: &str, revision: u32) -> Option<LicenceTerms> {
    if revision != COMMONS_REVISION {
        return None;
    }

    let (attribution_required, share_alike) = commons_evidence(short_name);

    Some(LicenceTerms {
        id: id.to_owned(),
        revision: COMMONS_REVISION,
        name: short_name.to_owned(),
        url: None,
        attribution_required,
        redistribution_allowed: true,
        cache_allowed: true,
        share_alike,
        non_commercial: false,
        storage_cap_bytes: None,
        terms_basis: Some(format!(
            "{COMMONS_ID} {short_name}: per-file licence from extmetadata"
        )),
        uncertainty: false,
    })
}

fn commons_evidence(short_name: &str) -> (bool, bool) {
    let upper = short_name.to_ascii_uppercase();
    let public_domain = upper == "CC0"
        || upper.starts_with("CC0 ")
        || upper.starts_with("PD")
        || upper.starts_with("PUBLIC DOMAIN");
    let share_alike = upper.contains("SA");

    (!public_domain, share_alike)
}

fn disagreement(id: &str, member: &'static str) -> UntrustedLicence {
    UntrustedLicence::Disagreement {
        id: id.to_owned(),
        member,
    }
}

fn builtin() -> Vec<LicenceTerms> {
    vec![
        LicenceTerms {
            id: COMMONS_ID.to_owned(),
            revision: REGISTRY_REVISION,
            name: "Wikimedia Commons".to_owned(),
            url: Some("https://commons.wikimedia.org/".to_owned()),
            attribution_required: true,
            redistribution_allowed: true,
            cache_allowed: true,
            share_alike: false,
            non_commercial: false,
            storage_cap_bytes: None,
            terms_basis: Some(
                "commons extmetadata: attribution and share-alike per file".to_owned(),
            ),
            uncertainty: false,
        },
        LicenceTerms {
            id: "musicbrainz".to_owned(),
            revision: REGISTRY_REVISION,
            name: "CC0 1.0 Universal".to_owned(),
            url: Some("https://creativecommons.org/publicdomain/zero/1.0/".to_owned()),
            attribution_required: false,
            redistribution_allowed: true,
            cache_allowed: true,
            share_alike: false,
            non_commercial: false,
            storage_cap_bytes: None,
            terms_basis: Some(
                "CC0 1.0 Universal: core data, released into the public domain".to_owned(),
            ),
            uncertainty: false,
        },
        LicenceTerms {
            id: "musicbrainz.supplementary".to_owned(),
            revision: REGISTRY_REVISION,
            name: "CC BY-NC-SA 3.0".to_owned(),
            url: Some("https://creativecommons.org/licenses/by-nc-sa/3.0/".to_owned()),
            attribution_required: true,
            redistribution_allowed: true,
            cache_allowed: true,
            share_alike: true,
            non_commercial: true,
            storage_cap_bytes: None,
            terms_basis: Some(
                "CC BY-NC-SA 3.0: supplementary data, attributed and non-commercial".to_owned(),
            ),
            uncertainty: false,
        },
        LicenceTerms {
            id: "wikipedia".to_owned(),
            revision: REGISTRY_REVISION,
            name: "CC BY-SA 4.0".to_owned(),
            url: Some("https://creativecommons.org/licenses/by-sa/4.0/".to_owned()),
            attribution_required: true,
            redistribution_allowed: true,
            cache_allowed: true,
            share_alike: true,
            non_commercial: false,
            storage_cap_bytes: None,
            terms_basis: Some(
                "CC BY-SA 4.0 and GFDL: attribution and a link back required".to_owned(),
            ),
            uncertainty: false,
        },
        LicenceTerms {
            id: "youtube_music".to_owned(),
            revision: REGISTRY_REVISION,
            name: "YouTube Terms of Service".to_owned(),
            url: Some("https://www.youtube.com/t/terms".to_owned()),
            attribution_required: false,
            redistribution_allowed: true,
            cache_allowed: true,
            share_alike: false,
            non_commercial: false,
            storage_cap_bytes: None,
            terms_basis: Some("YouTube Terms of Service: no redistribution granted".to_owned()),
            uncertainty: true,
        },
        LicenceTerms {
            id: "theaudiodb".to_owned(),
            revision: REGISTRY_REVISION,
            name: "TheAudioDB Terms".to_owned(),
            url: Some("https://www.theaudiodb.com/terms".to_owned()),
            attribution_required: true,
            redistribution_allowed: false,
            cache_allowed: true,
            share_alike: false,
            non_commercial: false,
            storage_cap_bytes: None,
            terms_basis: Some(
                "TheAudioDB terms grant copying of API content but not the photographs".to_owned(),
            ),
            uncertainty: false,
        },
        LicenceTerms {
            id: "listenbrainz".to_owned(),
            revision: REGISTRY_REVISION,
            name: "ListenBrainz Terms".to_owned(),
            url: Some("https://listenbrainz.org/terms/".to_owned()),
            attribution_required: true,
            redistribution_allowed: false,
            cache_allowed: true,
            share_alike: false,
            non_commercial: false,
            storage_cap_bytes: None,
            terms_basis: Some(
                "ListenBrainz output licence is not stated on a primary page".to_owned(),
            ),
            uncertainty: true,
        },
        LicenceTerms {
            id: "lastfm".to_owned(),
            revision: REGISTRY_REVISION,
            name: "Last.fm API Terms of Service".to_owned(),
            url: Some("https://www.last.fm/api/tos".to_owned()),
            attribution_required: true,
            redistribution_allowed: true,
            cache_allowed: true,
            share_alike: false,
            non_commercial: true,
            storage_cap_bytes: Some(100 * 1024 * 1024),
            terms_basis: Some(
                "Last.fm API ToS 4.1 with 3.1 non-commercial and 4.3.4 100 MB cap".to_owned(),
            ),
            uncertainty: false,
        },
    ]
}
