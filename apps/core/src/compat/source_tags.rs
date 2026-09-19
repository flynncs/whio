use crate::tracks::{ArtistCredit, ProviderKind, SourceIdentity};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SourceTags {
    #[default]
    Off,
    ArtistPrefix,
}

impl SourceTags {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Some(Self::Off),
            "artist" => Some(Self::ArtistPrefix),
            _ => None,
        }
    }

    pub(crate) fn artist(self, artists: &[ArtistCredit], sources: &[SourceIdentity]) -> String {
        let artists = artists
            .iter()
            .map(ArtistCredit::name)
            .collect::<Vec<_>>()
            .join(", ");

        if artists.is_empty() {
            return artists;
        }

        match self.marker(sources) {
            Some(marker) => format!("{marker} {artists}"),
            None => artists,
        }
    }

    fn marker(self, sources: &[SourceIdentity]) -> Option<String> {
        if self == Self::Off {
            return None;
        }

        let mut markers: Vec<&'static str> = Vec::new();
        for kind in sources
            .iter()
            .filter_map(|source| source.provider_id().kind())
        {
            let marker = shorthand(kind);
            if !markers.contains(&marker) {
                markers.push(marker);
            }
        }

        (!markers.is_empty()).then(|| format!("[{}]", markers.join("/")))
    }
}

fn shorthand(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Local => "LOCAL",
        ProviderKind::YoutubeMusic => "YTM",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::{ProviderId, SourceScope};

    fn source(provider: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).expect("valid provider"),
            SourceScope::Global,
            "external-1".to_owned(),
        )
        .expect("valid source")
    }

    fn artists() -> Vec<ArtistCredit> {
        vec![ArtistCredit::new("Daft Punk".to_owned(), None).expect("valid credit")]
    }

    #[test]
    fn parse_accepts_the_configured_placements_only() {
        assert_eq!(SourceTags::parse("off"), Some(SourceTags::Off));
        assert_eq!(
            SourceTags::parse(" Artist "),
            Some(SourceTags::ArtistPrefix)
        );
        assert_eq!(SourceTags::parse("album"), None);
        assert_eq!(SourceTags::parse(""), None);
    }

    #[test]
    fn off_leaves_the_artist_untouched() {
        assert_eq!(
            SourceTags::Off.artist(&artists(), &[source("youtube_music")]),
            "Daft Punk"
        );
    }

    #[test]
    fn the_artist_prefix_names_each_provider_once() {
        let tags = SourceTags::ArtistPrefix;

        assert_eq!(
            tags.artist(&artists(), &[source("youtube_music")]),
            "[YTM] Daft Punk"
        );
        assert_eq!(
            tags.artist(
                &artists(),
                &[source("youtube_music"), source("youtube_music")]
            ),
            "[YTM] Daft Punk"
        );
    }

    #[test]
    fn every_artist_joins_before_the_marker() {
        let artists = vec![
            ArtistCredit::new("Daft Punk".to_owned(), None).unwrap(),
            ArtistCredit::new("Julian Casablancas".to_owned(), None).unwrap(),
        ];

        assert_eq!(
            SourceTags::ArtistPrefix.artist(&artists, &[source("youtube_music")]),
            "[YTM] Daft Punk, Julian Casablancas"
        );
    }

    #[test]
    fn an_unrecognised_provider_adds_no_marker() {
        assert_eq!(
            SourceTags::ArtistPrefix.artist(&artists(), &[source("somewhere_else")]),
            "Daft Punk"
        );
    }

    #[test]
    fn a_local_source_is_marked_and_can_join_a_remote_one() {
        let local = SourceIdentity::new(
            ProviderId::new("local".to_owned()).unwrap(),
            SourceScope::Named("/tank/data/media/music".to_owned()),
            "recording-1".to_owned(),
        )
        .unwrap();

        assert_eq!(
            SourceTags::ArtistPrefix.artist(&artists(), std::slice::from_ref(&local)),
            "[LOCAL] Daft Punk"
        );
        assert_eq!(
            SourceTags::ArtistPrefix.artist(&artists(), &[local, source("youtube_music")]),
            "[LOCAL/YTM] Daft Punk"
        );
    }
}
