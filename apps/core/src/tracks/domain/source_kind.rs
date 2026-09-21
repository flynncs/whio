use super::{LOCAL_PROVIDER_ID, ProviderId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Library,
    Provider,
}

impl SourceKind {
    pub fn of(provider_id: &ProviderId) -> Self {
        if ProviderRegistry::builtin().is_library(provider_id) {
            Self::Library
        } else {
            Self::Provider
        }
    }

    pub fn is_library(self) -> bool {
        self == Self::Library
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub kind: SourceKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderRegistry {
    descriptors: &'static [ProviderDescriptor],
}

const BUILTIN: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: LOCAL_PROVIDER_ID,
        kind: SourceKind::Library,
    },
    ProviderDescriptor {
        id: super::YOUTUBE_MUSIC_PROVIDER_ID,
        kind: SourceKind::Provider,
    },
];

impl ProviderRegistry {
    pub fn builtin() -> Self {
        Self {
            descriptors: BUILTIN,
        }
    }

    pub fn kind(&self, provider_id: &ProviderId) -> SourceKind {
        self.descriptors
            .iter()
            .find(|descriptor| descriptor.id == provider_id.as_str())
            .map_or(SourceKind::Provider, |descriptor| descriptor.kind)
    }

    pub fn is_library(&self, provider_id: &ProviderId) -> bool {
        self.kind(provider_id).is_library()
    }

    pub fn library_ids(&self) -> Vec<&'static str> {
        self.descriptors
            .iter()
            .filter(|descriptor| descriptor.kind.is_library())
            .map(|descriptor| descriptor.id)
            .collect()
    }
}

impl Default for ProviderRegistry {
    fn default() -> Self {
        Self::builtin()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> ProviderId {
        ProviderId::new(value.to_owned()).unwrap()
    }

    #[test]
    fn the_local_provider_is_a_library_source_and_youtube_is_not() {
        let registry = ProviderRegistry::builtin();

        assert_eq!(registry.kind(&id("local")), SourceKind::Library);
        assert_eq!(registry.kind(&id("youtube_music")), SourceKind::Provider);
        assert_eq!(registry.kind(&id("navidrome")), SourceKind::Provider);
        assert_eq!(registry.kind(&id("somewhere_else")), SourceKind::Provider);
    }

    #[test]
    fn only_the_local_provider_is_a_library_source() {
        assert_eq!(ProviderRegistry::builtin().library_ids(), vec!["local"]);
    }

    #[test]
    fn source_kind_classifies_without_a_registry_argument() {
        assert!(SourceKind::of(&id("local")).is_library());
        assert!(!SourceKind::of(&id("youtube_music")).is_library());
    }
}
