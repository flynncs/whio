use sha2::{Digest, Sha256};

use super::{ArtistId, SourceIdentity, SourceScope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrichmentSubject {
    artist_id: ArtistId,
    name: String,
    musicbrainz_artist_id: Option<String>,
    identity_revision: String,
    identities: Vec<SourceIdentity>,
}

impl EnrichmentSubject {
    pub fn new(
        artist_id: ArtistId,
        name: String,
        musicbrainz_artist_id: Option<String>,
        mut identities: Vec<SourceIdentity>,
    ) -> Self {
        identities.sort_by_key(member);
        let members = identities.iter().map(member).collect::<Vec<_>>();
        let identity_revision = identity_revision(musicbrainz_artist_id.as_deref(), &members);

        Self {
            artist_id,
            name,
            musicbrainz_artist_id,
            identity_revision,
            identities,
        }
    }

    pub fn artist_id(&self) -> &ArtistId {
        &self.artist_id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn musicbrainz_artist_id(&self) -> Option<&str> {
        self.musicbrainz_artist_id.as_deref()
    }

    pub fn identity_revision(&self) -> &str {
        &self.identity_revision
    }

    pub fn identities(&self) -> &[SourceIdentity] {
        &self.identities
    }
}

pub fn identity_revision(
    musicbrainz_artist_id: Option<&str>,
    sources: &[(String, String, String)],
) -> String {
    let mut sorted = sources.to_vec();
    sorted.sort();

    let mut basis = musicbrainz_artist_id.unwrap_or_default().to_owned();

    for (position, (provider_id, source_scope, external_id)) in sorted.iter().enumerate() {
        if position > 0 {
            basis.push('\u{1e}');
        }
        basis.push_str(provider_id);
        basis.push('\u{1f}');
        basis.push_str(source_scope);
        basis.push('\u{1f}');
        basis.push_str(external_id);
    }

    hex::encode(Sha256::digest(basis.as_bytes()))
}

fn member(identity: &SourceIdentity) -> (String, String, String) {
    (
        identity.provider_id().as_str().to_owned(),
        scope_member(identity.source_scope()),
        identity.external_id().to_owned(),
    )
}

fn scope_member(scope: &SourceScope) -> String {
    match scope {
        SourceScope::Global => "global".to_owned(),
        SourceScope::Named(name) => format!("named:{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::music_library::domain::ProviderId;

    fn source(provider_id: &str, scope: &str, external_id: &str) -> SourceIdentity {
        let scope = match scope {
            "global" => SourceScope::Global,
            name => SourceScope::Named(name.to_owned()),
        };

        SourceIdentity::new(
            ProviderId::new(provider_id.to_owned()).unwrap(),
            scope,
            external_id.to_owned(),
        )
        .unwrap()
    }

    fn subject(mbid: Option<&str>, identities: Vec<SourceIdentity>) -> EnrichmentSubject {
        EnrichmentSubject::new(
            ArtistId::new(),
            "Daft Punk".to_owned(),
            mbid.map(str::to_owned),
            identities,
        )
    }

    fn triple(
        provider_id: &str,
        source_scope: &str,
        external_id: &str,
    ) -> (String, String, String) {
        (
            provider_id.to_owned(),
            source_scope.to_owned(),
            external_id.to_owned(),
        )
    }

    #[test]
    fn a_subject_sorts_its_identities_by_provider_scope_and_external_id() {
        let subject = subject(
            Some("mbid-daft-punk"),
            vec![
                source("youtube_music", "global", "b"),
                source("local", "named:/music", "a"),
                source("local", "global", "z"),
            ],
        );

        let expected = vec![
            source("local", "global", "z"),
            source("local", "named:/music", "a"),
            source("youtube_music", "global", "b"),
        ];

        assert_eq!(subject.identities(), expected.as_slice());
    }

    #[test]
    fn the_revision_depends_on_the_basis_and_not_on_input_order() {
        let ordered = subject(
            Some("mbid-daft-punk"),
            vec![
                source("local", "global", "z"),
                source("youtube_music", "global", "b"),
            ],
        );
        let reversed = subject(
            Some("mbid-daft-punk"),
            vec![
                source("youtube_music", "global", "b"),
                source("local", "global", "z"),
            ],
        );

        assert_eq!(ordered.identity_revision(), reversed.identity_revision());
        assert_eq!(ordered.identities(), reversed.identities());
    }

    #[test]
    fn the_pure_revision_matches_the_subject_it_built() {
        let identities = vec![
            source("youtube_music", "global", "channel-1"),
            source("local", "named:/music", "track-2"),
        ];
        let subject = subject(Some("mbid-daft-punk"), identities.clone());
        let members = identities.iter().map(member).collect::<Vec<_>>();

        assert_eq!(
            identity_revision(Some("mbid-daft-punk"), &members),
            subject.identity_revision()
        );
        assert_eq!(
            identity_revision(Some("mbid-daft-punk"), &members),
            identity_revision(
                Some("mbid-daft-punk"),
                &members.into_iter().rev().collect::<Vec<_>>()
            )
        );
    }

    #[test]
    fn the_revision_changes_with_every_basis_member() {
        let base = subject(
            Some("mbid-daft-punk"),
            vec![source("youtube_music", "global", "channel-1")],
        );

        let changed_identifier = subject(
            Some("mbid-other"),
            vec![source("youtube_music", "global", "channel-1")],
        );
        let changed_provider = subject(
            Some("mbid-daft-punk"),
            vec![source("local", "global", "channel-1")],
        );
        let changed_scope = subject(
            Some("mbid-daft-punk"),
            vec![source("youtube_music", "named:/music", "channel-1")],
        );
        let changed_external_id = subject(
            Some("mbid-daft-punk"),
            vec![source("youtube_music", "global", "channel-2")],
        );
        let added_association = subject(
            Some("mbid-daft-punk"),
            vec![
                source("youtube_music", "global", "channel-1"),
                source("local", "global", "track-1"),
            ],
        );
        let removed_identifier =
            subject(None, vec![source("youtube_music", "global", "channel-1")]);

        for (mutated, label) in [
            (changed_identifier, "musicbrainz_artist_id"),
            (changed_provider, "provider_id"),
            (changed_scope, "source_scope"),
            (changed_external_id, "external_id"),
            (added_association, "identities"),
            (removed_identifier, "absent musicbrainz_artist_id"),
        ] {
            assert_ne!(
                base.identity_revision(),
                mutated.identity_revision(),
                "{label}"
            );
        }

        assert_ne!(
            base.identity_revision(),
            identity_revision(
                Some("mbid-daft-punk"),
                &[triple("local", "global", "channel-1")]
            )
        );
    }

    #[test]
    fn an_artist_without_an_identifier_or_sources_still_has_a_revision() {
        let unidentified = subject(None, Vec::new());

        assert!(unidentified.identities().is_empty());
        assert_eq!(unidentified.musicbrainz_artist_id(), None);
        assert_eq!(unidentified.identity_revision().len(), 64);
        assert!(
            unidentified
                .identity_revision()
                .chars()
                .all(|character| character.is_ascii_hexdigit())
        );
        assert_ne!(
            unidentified.identity_revision(),
            subject(Some("mbid-daft-punk"), Vec::new()).identity_revision()
        );
    }
}
