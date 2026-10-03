use crate::features::metadata::domain::{FieldId, StoredFact};

pub const YOUTUBE_MUSIC: &str = "youtube_music";
pub const THEAUDIODB: &str = "theaudiodb";
pub const MUSICBRAINZ: &str = "musicbrainz";
pub const WIKIPEDIA: &str = "wikipedia";
pub const COMMONS: &str = "commons";
pub const LISTENBRAINZ: &str = "listenbrainz";
pub const LASTFM: &str = "lastfm";
pub const LOCAL: &str = "local";

pub const ADDRESSING_PROVIDERS: [&str; 1] = [YOUTUBE_MUSIC];

pub const METADATA_PROVIDERS: [&str; 6] = [
    THEAUDIODB,
    MUSICBRAINZ,
    WIKIPEDIA,
    COMMONS,
    LISTENBRAINZ,
    LASTFM,
];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FieldOrder {
    orders: Vec<(FieldId, Vec<String>)>,
}

impl FieldOrder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_entries(entries: impl IntoIterator<Item = (FieldId, Vec<String>)>) -> Self {
        Self {
            orders: entries.into_iter().collect(),
        }
    }

    pub fn set(&mut self, field: FieldId, sources: Vec<String>) {
        match self.orders.iter_mut().find(|(entry, _)| *entry == field) {
            Some((_, current)) => *current = sources,
            None => self.orders.push((field, sources)),
        }
    }

    pub fn sources(&self, field: FieldId) -> &[String] {
        self.configured(field).unwrap_or(&[])
    }

    pub fn configured(&self, field: FieldId) -> Option<&[String]> {
        self.orders
            .iter()
            .find(|(entry, _)| *entry == field)
            .map(|(_, sources)| sources.as_slice())
    }

    pub fn defaults() -> Self {
        let sources =
            |sources: &[&str]| sources.iter().map(|source| (*source).to_owned()).collect();

        Self::from_entries([
            (FieldId::IdentityName, sources(&[MUSICBRAINZ, LOCAL])),
            (FieldId::IdentityMbid, sources(&[LOCAL, MUSICBRAINZ])),
            (FieldId::IdentityDescription, sources(&[WIKIPEDIA])),
            (
                FieldId::BiographyContent,
                sources(&[YOUTUBE_MUSIC, THEAUDIODB, WIKIPEDIA, LASTFM]),
            ),
            (FieldId::BiographySummary, sources(&[WIKIPEDIA])),
            (FieldId::BiographyLanguage, sources(&[])),
            (FieldId::ImagePortrait, sources(&[THEAUDIODB, COMMONS])),
            (
                FieldId::ImageWide,
                sources(&[THEAUDIODB, YOUTUBE_MUSIC, COMMONS]),
            ),
            (FieldId::PageUrl, sources(&[MUSICBRAINZ])),
            (FieldId::Tag, sources(&[MUSICBRAINZ, LISTENBRAINZ])),
            (
                FieldId::Similar,
                sources(&[YOUTUBE_MUSIC, LISTENBRAINZ, LOCAL]),
            ),
            (
                FieldId::TopTracks,
                sources(&[YOUTUBE_MUSIC, LISTENBRAINZ, LOCAL]),
            ),
            (FieldId::MetricListeners, sources(&[LASTFM])),
            (FieldId::MetricPlays, sources(&[LASTFM])),
            (FieldId::MetricListen, sources(&[LISTENBRAINZ])),
            (FieldId::MetricListener, sources(&[LISTENBRAINZ])),
        ])
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Selection<'a> {
    pub winner: Option<&'a StoredFact>,
    pub disagreements: Vec<&'a StoredFact>,
}

impl Selection<'_> {
    pub fn contested(&self) -> bool {
        !self.disagreements.is_empty()
    }
}

pub fn select<'a>(facts: &'a [StoredFact], order: &FieldOrder) -> Selection<'a> {
    let Some(field) = facts.first().map(|fact| fact.field) else {
        return Selection {
            winner: None,
            disagreements: Vec::new(),
        };
    };
    let Some(configured) = order
        .configured(field)
        .filter(|sources| !sources.is_empty())
    else {
        return Selection {
            winner: None,
            disagreements: Vec::new(),
        };
    };
    let present = ranked(
        facts
            .iter()
            .filter(|fact| configured.contains(&fact.source.provider_id)),
        configured,
    );
    let mut present = present.into_iter();
    let winner = present.next();

    Selection {
        winner,
        disagreements: present.collect(),
    }
}

fn ranked<'a>(
    facts: impl Iterator<Item = &'a StoredFact>,
    configured: &[String],
) -> Vec<&'a StoredFact> {
    let facts = facts.collect::<Vec<_>>();

    configured
        .iter()
        .flat_map(|provider_id| {
            facts
                .iter()
                .filter(move |fact| fact.source.provider_id == *provider_id)
                .copied()
        })
        .collect()
}
