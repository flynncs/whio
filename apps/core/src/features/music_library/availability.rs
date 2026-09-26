use super::domain::{
    Availability, LOCAL_PROVIDER_ID, ProviderId, SourceIdentity, SourceKind,
    YOUTUBE_MUSIC_PROVIDER_ID,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderDescriptor {
    pub id: &'static str,
    pub kind: SourceKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRegistry {
    descriptors: &'static [ProviderDescriptor],
    enabled: Vec<&'static str>,
}

const BUILTIN: &[ProviderDescriptor] = &[
    ProviderDescriptor {
        id: LOCAL_PROVIDER_ID,
        kind: SourceKind::Library,
    },
    ProviderDescriptor {
        id: YOUTUBE_MUSIC_PROVIDER_ID,
        kind: SourceKind::Provider,
    },
];

impl ProviderRegistry {
    pub fn builtin() -> Self {
        Self::with_enabled(
            BUILTIN
                .iter()
                .filter(|descriptor| descriptor.kind == SourceKind::Provider)
                .map(|descriptor| descriptor.id),
        )
    }

    pub fn with_enabled(enabled: impl IntoIterator<Item = &'static str>) -> Self {
        Self {
            descriptors: BUILTIN,
            enabled: enabled.into_iter().collect(),
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

    pub fn is_enabled(&self, provider_id: &ProviderId) -> bool {
        self.enabled.iter().any(|id| *id == provider_id.as_str())
    }

    pub fn library_ids(&self) -> Vec<&'static str> {
        self.descriptors
            .iter()
            .filter(|descriptor| descriptor.kind.is_library())
            .map(|descriptor| descriptor.id)
            .collect()
    }

    pub fn availability(&self, sources: &[SourceIdentity]) -> Availability {
        if sources
            .iter()
            .any(|source| self.is_library(source.provider_id()))
        {
            return Availability::Owned;
        }

        if sources
            .iter()
            .any(|source| self.is_enabled(source.provider_id()))
        {
            return Availability::ProviderAvailable;
        }

        Availability::Unavailable
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
    use crate::features::music_library::domain::SourceScope;

    fn id(value: &str) -> ProviderId {
        ProviderId::new(value.to_owned()).unwrap()
    }

    fn source(provider: &str, external_id: &str) -> SourceIdentity {
        SourceIdentity::new(id(provider), SourceScope::Global, external_id.to_owned()).unwrap()
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
    fn an_owned_source_is_owned_however_the_registry_is_configured() {
        let registry = ProviderRegistry::with_enabled(Vec::new());

        assert_eq!(
            registry.availability(&[source("local", "path-a")]),
            Availability::Owned
        );
        assert_eq!(
            registry.availability(&[source("local", "path-a"), source("youtube_music", "vid-1")]),
            Availability::Owned
        );
    }

    #[test]
    fn a_provider_source_is_available_only_while_its_provider_is_enabled() {
        let enabled = ProviderRegistry::with_enabled(["youtube_music"]);
        let disabled = ProviderRegistry::with_enabled(Vec::new());

        assert_eq!(
            enabled.availability(&[source("youtube_music", "vid-1")]),
            Availability::ProviderAvailable
        );
        assert_eq!(
            disabled.availability(&[source("youtube_music", "vid-1")]),
            Availability::Unavailable
        );
        assert_eq!(disabled.availability(&[]), Availability::Unavailable);
        assert_eq!(
            disabled.availability(&[source("somewhere_else", "x")]),
            Availability::Unavailable
        );
    }
}
