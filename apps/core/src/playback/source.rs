use std::cmp::Ordering;

use uuid::Uuid;

use crate::tracks::{ProviderKind, SourceIdentity, SourceScope};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackSource {
    Navidrome(NavidromeSource),
    Youtube(YoutubeSource),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavidromeSource {
    identity: SourceIdentity,
    server_id: Uuid,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YoutubeSource {
    identity: SourceIdentity,
}

impl PlaybackSource {
    pub fn from_identity(identity: SourceIdentity) -> Option<Self> {
        match (identity.provider_id().kind()?, identity.source_scope()) {
            (ProviderKind::Navidrome, SourceScope::Named(scope)) => {
                Uuid::parse_str(scope).ok().map(|server_id| {
                    Self::Navidrome(NavidromeSource {
                        identity,
                        server_id,
                    })
                })
            }
            (ProviderKind::YoutubeMusic, SourceScope::Global) => {
                Some(Self::Youtube(YoutubeSource { identity }))
            }
            _ => None,
        }
    }

    pub fn kind(&self) -> ProviderKind {
        match self {
            Self::Navidrome(_) => ProviderKind::Navidrome,
            Self::Youtube(_) => ProviderKind::YoutubeMusic,
        }
    }

    pub fn identity(&self) -> &SourceIdentity {
        match self {
            Self::Navidrome(source) => &source.identity,
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
                (Self::Navidrome(left), Self::Navidrome(right)) => {
                    left.server_id.cmp(&right.server_id).then_with(|| {
                        left.identity
                            .external_id()
                            .cmp(right.identity.external_id())
                    })
                }
                (Self::Youtube(left), Self::Youtube(right)) => left
                    .identity
                    .external_id()
                    .cmp(right.identity.external_id()),
                _ => Ordering::Equal,
            })
    }
}

impl NavidromeSource {
    pub fn identity(&self) -> &SourceIdentity {
        &self.identity
    }

    pub fn server_id(&self) -> &Uuid {
        &self.server_id
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
        let server_id = Uuid::now_v7();
        let navidrome = identity(
            "navidrome",
            SourceScope::Named(server_id.to_string()),
            "song-1",
        );
        let youtube = identity("youtube_music", SourceScope::Global, "video-1");
        let unknown = identity("other_provider", SourceScope::Global, "item-1");

        assert!(matches!(
            PlaybackSource::from_identity(navidrome),
            Some(PlaybackSource::Navidrome(_))
        ));
        assert!(matches!(
            PlaybackSource::from_identity(youtube),
            Some(PlaybackSource::Youtube(_))
        ));
        assert!(PlaybackSource::from_identity(unknown).is_none());
    }

    #[test]
    fn orders_navidrome_before_youtube_and_servers_deterministically() {
        let first = Uuid::from_u128(1);
        let second = Uuid::from_u128(2);
        let youtube = PlaybackSource::from_identity(identity(
            "youtube_music",
            SourceScope::Global,
            "video-1",
        ))
        .unwrap();
        let second_server = PlaybackSource::from_identity(identity(
            "navidrome",
            SourceScope::Named(second.to_string()),
            "song-1",
        ))
        .unwrap();
        let first_server = PlaybackSource::from_identity(identity(
            "navidrome",
            SourceScope::Named(first.to_string()),
            "song-1",
        ))
        .unwrap();

        assert_eq!(
            PlaybackSource::compare(&first_server, &second_server),
            Ordering::Less
        );
        assert_eq!(
            PlaybackSource::compare(&second_server, &youtube),
            Ordering::Less
        );
    }
}
