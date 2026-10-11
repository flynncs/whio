use std::cmp::Ordering;

use crate::features::music_library::domain::{ProviderKind, SourceIdentity, SourceScope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackSource {
    Local(LocalSource),
    Youtube(YoutubeSource),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceKey(SourceIdentity);

impl SourceKey {
    pub fn new(source: &PlaybackSource) -> Self {
        Self(source.identity().clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalSource {
    identity: SourceIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YoutubeSource {
    identity: SourceIdentity,
}

impl PlaybackSource {
    pub fn from_identity(identity: SourceIdentity) -> Option<Self> {
        match (identity.provider_id().kind()?, identity.source_scope()) {
            (ProviderKind::Local, SourceScope::Named(_)) => {
                Some(Self::Local(LocalSource { identity }))
            }
            (ProviderKind::YoutubeMusic, SourceScope::Global) => {
                Some(Self::Youtube(YoutubeSource { identity }))
            }
            _ => None,
        }
    }

    pub fn kind(&self) -> ProviderKind {
        match self {
            Self::Local(_) => ProviderKind::Local,
            Self::Youtube(_) => ProviderKind::YoutubeMusic,
        }
    }

    pub fn identity(&self) -> &SourceIdentity {
        match self {
            Self::Local(source) => &source.identity,
            Self::Youtube(source) => &source.identity,
        }
    }

    pub(crate) fn rank(&self) -> u8 {
        self.kind().precedence()
    }

    pub(crate) fn compare(left: &Self, right: &Self) -> Ordering {
        left.rank().cmp(&right.rank()).then_with(|| {
            left.identity()
                .external_id()
                .cmp(right.identity().external_id())
        })
    }
}

impl LocalSource {
    pub fn identity(&self) -> &SourceIdentity {
        &self.identity
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
    use crate::features::music_library::domain::ProviderId;

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
        let local = identity(
            "local",
            SourceScope::Named("/tank/data/media/music".to_owned()),
            "recording-1",
        );
        let unknown = identity("other_provider", SourceScope::Global, "item-1");

        assert!(matches!(
            PlaybackSource::from_identity(youtube),
            Some(PlaybackSource::Youtube(_))
        ));
        assert!(matches!(
            PlaybackSource::from_identity(local),
            Some(PlaybackSource::Local(_))
        ));
        assert!(PlaybackSource::from_identity(unknown).is_none());
    }

    #[test]
    fn a_local_source_must_name_the_root_it_was_scanned_under() {
        let unscoped = identity("local", SourceScope::Global, "recording-1");

        assert!(PlaybackSource::from_identity(unscoped).is_none());
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

    #[test]
    fn a_local_source_outranks_a_remote_one() {
        let local = PlaybackSource::from_identity(identity(
            "local",
            SourceScope::Named("/music".to_owned()),
            "recording-1",
        ))
        .unwrap();
        let youtube = PlaybackSource::from_identity(identity(
            "youtube_music",
            SourceScope::Global,
            "video-9",
        ))
        .unwrap();

        assert_eq!(PlaybackSource::compare(&local, &youtube), Ordering::Less);
        assert_eq!(PlaybackSource::compare(&youtube, &local), Ordering::Greater);
    }
}
