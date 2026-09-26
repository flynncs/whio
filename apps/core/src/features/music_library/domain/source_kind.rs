use super::{LOCAL_PROVIDER_ID, ProviderId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Library,
    Provider,
}

impl SourceKind {
    pub fn of(provider_id: &ProviderId) -> Self {
        if provider_id.as_str() == LOCAL_PROVIDER_ID {
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
pub enum Availability {
    Owned,
    ProviderAvailable,
    Unavailable,
}

impl Availability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::ProviderAvailable => "available",
            Self::Unavailable => "unavailable",
        }
    }

    pub fn is_owned(self) -> bool {
        self == Self::Owned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> ProviderId {
        ProviderId::new(value.to_owned()).unwrap()
    }

    #[test]
    fn source_kind_classifies_without_a_registry_argument() {
        assert!(SourceKind::of(&id("local")).is_library());
        assert!(!SourceKind::of(&id("youtube_music")).is_library());
    }

    #[test]
    fn availability_reports_its_wire_name_and_ownership() {
        assert_eq!(Availability::Owned.as_str(), "owned");
        assert_eq!(Availability::ProviderAvailable.as_str(), "available");
        assert_eq!(Availability::Unavailable.as_str(), "unavailable");
        assert!(Availability::Owned.is_owned());
        assert!(!Availability::Unavailable.is_owned());
    }
}
