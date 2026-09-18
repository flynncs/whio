use std::cmp::Ordering;

use crate::tracks::{ProviderKind, SourceIdentity, SourceScope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackSource {
    Youtube(YoutubeSource),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YoutubeSource {
    identity: SourceIdentity,
}

impl PlaybackSource {
    pub fn from_identity(identity: SourceIdentity) -> Option<Self> {
        match (identity.provider_id().kind()?, identity.source_scope()) {
            (ProviderKind::YoutubeMusic, SourceScope::Global) => {
                Some(Self::Youtube(YoutubeSource { identity }))
            }
            _ => None,
        }
    }

    pub fn kind(&self) -> ProviderKind {
        match self {
            Self::Youtube(_) => ProviderKind::YoutubeMusic,
        }
    }

    pub fn identity(&self) -> &SourceIdentity {
        match self {
            Self::Youtube(source) => &source.identity,
        }
    }

    pub(crate) fn rank(&self) -> u8 {
        self.kind().precedence()
    }

    pub(crate) fn compare(left: &Self, right: &Self) -> Ordering {
        left.rank()
            .cmp(&right.rank())
            .then_with(|| match (left, right) {
                (Self::Youtube(left), Self::Youtube(right)) => left
                    .identity
                    .external_id()
                    .cmp(right.identity.external_id()),
            })
    }
}

impl YoutubeSource {
    pub fn identity(&self) -> &SourceIdentity {
        &self.identity
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tracks::ProviderId;

    fn identity(provider: &str, scope: SourceScope, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(
            ProviderId::new(provider.to_owned()).unwrap(),
            scope,
            external_id.to_owned(),
        )
        .unwrap()
    }

    #[test]
    fn classifies_supported_sources_and_rejects_unknown_shapes() {
        let youtube = identity("youtube_music", SourceScope::Global, "video-1");
        let unknown = identity("other_provider", SourceScope::Global, "item-1");

        assert!(matches!(
            PlaybackSource::from_identity(youtube),
            Some(PlaybackSource::Youtube(_))
        ));
        assert!(PlaybackSource::from_identity(unknown).is_none());
    }

    #[test]
    fn orders_sources_deterministically_by_external_id() {
        let first = PlaybackSource::from_identity(identity(
            "youtube_music",
            SourceScope::Global,
            "video-1",
        ))
        .unwrap();
        let second = PlaybackSource::from_identity(identity(
            "youtube_music",
            SourceScope::Global,
            "video-2",
        ))
        .unwrap();

        assert_eq!(PlaybackSource::compare(&first, &second), Ordering::Less);
        assert_eq!(PlaybackSource::compare(&second, &first), Ordering::Greater);
        assert_eq!(PlaybackSource::compare(&first, &first), Ordering::Equal);
    }
}
