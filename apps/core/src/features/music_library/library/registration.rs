use std::{
    fmt::{self, Display},
    path::PathBuf,
    str::FromStr,
    sync::Arc,
};

use uuid::Uuid;

use crate::features::music_library::domain::IdSet;
use crate::features::music_library::storage::contracts::{LibraryStore, TrackRepositoryError};

pub const DEFAULT_LABEL: &str = "default";
pub const MAX_LABEL_LENGTH: usize = 100;

pub(crate) fn is_label_token(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_LABEL_LENGTH
        && !value.contains('/')
        && !value.chars().any(char::is_whitespace)
}

pub(crate) fn deduplicated_roots(registrations: &[LibraryRegistration]) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();

    for registration in registrations {
        for root in registration.roots() {
            if !roots.contains(root) {
                roots.push(root.clone());
            }
        }
    }

    roots
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LibraryId(Uuid);

impl LibraryId {
    pub fn from_uuid(value: Uuid) -> Self {
        Self(value)
    }

    pub fn as_uuid(&self) -> &Uuid {
        &self.0
    }
}

impl Display for LibraryId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

impl FromStr for LibraryId {
    type Err = uuid::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::parse_str(value)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Library {
    id: LibraryId,
    numeric_id: i32,
    label: String,
}

impl Library {
    pub fn new(id: LibraryId, numeric_id: i32, label: String) -> Self {
        Self {
            id,
            numeric_id,
            label,
        }
    }

    pub fn id(&self) -> &LibraryId {
        &self.id
    }

    pub fn numeric_id(&self) -> i32 {
        self.numeric_id
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibraryRegistration {
    label: String,
    roots: Vec<PathBuf>,
}

impl LibraryRegistration {
    pub fn new(label: String, roots: Vec<PathBuf>) -> Self {
        Self { label, roots }
    }

    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LibraryRegistrationError {
    #[error("library registry is not configured")]
    Disabled,

    #[error("library store failed")]
    Store(#[source] TrackRepositoryError),

    #[error("label `{label}` is not registered")]
    UnknownLabel { label: String },

    #[error("no library is registered as {numeric_id}")]
    UnknownNumericId { numeric_id: i32 },

    #[error("library {id} is not registered")]
    UnknownLibrary { id: String },
}

pub struct LibraryRegistry {
    store: Option<Arc<dyn LibraryStore>>,
}

impl LibraryRegistry {
    pub fn new(store: Arc<dyn LibraryStore>) -> Self {
        Self { store: Some(store) }
    }

    pub fn disabled() -> Self {
        Self { store: None }
    }

    fn store(&self) -> Result<&Arc<dyn LibraryStore>, LibraryRegistrationError> {
        self.store
            .as_ref()
            .ok_or(LibraryRegistrationError::Disabled)
    }

    pub async fn register(
        &self,
        registrations: &[LibraryRegistration],
    ) -> Result<Vec<Library>, LibraryRegistrationError> {
        let store = self.store()?;

        for registration in registrations {
            let existing = store
                .find_by_label(registration.label())
                .await
                .map_err(LibraryRegistrationError::Store)?;

            match existing {
                Some(library) => store
                    .replace_roots(library.id(), registration.roots())
                    .await
                    .map_err(LibraryRegistrationError::Store)?,
                None => {
                    store
                        .insert_library(registration.label(), registration.roots())
                        .await
                        .map_err(LibraryRegistrationError::Store)?;
                }
            }
        }

        store
            .list_libraries()
            .await
            .map_err(LibraryRegistrationError::Store)
    }

    pub async fn libraries(&self) -> Result<Vec<Library>, LibraryRegistrationError> {
        self.store()?
            .list_libraries()
            .await
            .map_err(LibraryRegistrationError::Store)
    }

    pub(crate) async fn ids_for_numeric(
        &self,
        numeric_ids: &[i32],
    ) -> Result<Option<IdSet<LibraryId>>, LibraryRegistrationError> {
        let store = self.store()?;
        let known = store
            .list_libraries()
            .await
            .map_err(LibraryRegistrationError::Store)?;
        let mut selected = Vec::with_capacity(numeric_ids.len());

        for numeric_id in numeric_ids {
            match known
                .iter()
                .find(|library| library.numeric_id() == *numeric_id)
            {
                Some(library) => selected.push(library.id().clone()),
                None => return Ok(None),
            }
        }

        Ok(IdSet::new(selected).ok())
    }

    #[allow(dead_code)]
    pub(crate) async fn scopes_for(
        &self,
        label: &str,
    ) -> Result<Vec<String>, LibraryRegistrationError> {
        let store = self.store()?;
        let library = store
            .find_by_label(label)
            .await
            .map_err(LibraryRegistrationError::Store)?
            .ok_or_else(|| LibraryRegistrationError::UnknownLabel {
                label: label.to_owned(),
            })?;

        store
            .scopes_for_ids(std::slice::from_ref(library.id()))
            .await
            .map_err(LibraryRegistrationError::Store)
    }

    #[allow(dead_code)]
    pub(crate) async fn scopes_for_numeric(
        &self,
        numeric_id: i32,
    ) -> Result<Vec<String>, LibraryRegistrationError> {
        let ids = self
            .ids_for_numeric(std::slice::from_ref(&numeric_id))
            .await?
            .ok_or(LibraryRegistrationError::UnknownNumericId { numeric_id })?;
        let ids = ids.iter().cloned().collect::<Vec<_>>();

        self.store()?
            .scopes_for_ids(&ids)
            .await
            .map_err(LibraryRegistrationError::Store)
    }

    #[allow(dead_code)]
    pub(crate) async fn scopes_for_ids(
        &self,
        ids: &[LibraryId],
    ) -> Result<Vec<String>, LibraryRegistrationError> {
        let store = self.store()?;
        let known = store
            .list_libraries()
            .await
            .map_err(LibraryRegistrationError::Store)?;

        for id in ids {
            if !known.iter().any(|library| library.id() == id) {
                return Err(LibraryRegistrationError::UnknownLibrary { id: id.to_string() });
            }
        }

        store
            .scopes_for_ids(ids)
            .await
            .map_err(LibraryRegistrationError::Store)
    }

    pub(crate) fn scan_roots(&self, registrations: &[LibraryRegistration]) -> Vec<PathBuf> {
        deduplicated_roots(registrations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct UnavailableLibraryStore;

    #[async_trait::async_trait]
    impl LibraryStore for UnavailableLibraryStore {
        async fn find_by_label(
            &self,
            _label: &str,
        ) -> Result<Option<Library>, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn insert_library(
            &self,
            _label: &str,
            _roots: &[PathBuf],
        ) -> Result<Library, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn replace_roots(
            &self,
            _library_id: &LibraryId,
            _roots: &[PathBuf],
        ) -> Result<(), TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn list_libraries(&self) -> Result<Vec<Library>, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }

        async fn scopes_for_ids(
            &self,
            _ids: &[LibraryId],
        ) -> Result<Vec<String>, TrackRepositoryError> {
            Err(TrackRepositoryError::Unavailable)
        }
    }

    fn registration(label: &str, roots: &[&str]) -> LibraryRegistration {
        LibraryRegistration::new(label.to_owned(), roots.iter().map(PathBuf::from).collect())
    }

    #[test]
    fn scan_roots_deduplicates_roots_shared_by_labels() {
        let registry = LibraryRegistry::disabled();
        let registrations = vec![
            registration("music", &["/a", "/b"]),
            registration("books", &["/a", "/c"]),
            registration("default", &["/b"]),
        ];

        assert_eq!(
            registry.scan_roots(&registrations),
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
    }

    #[tokio::test]
    async fn an_unavailable_store_fails_registration() {
        let registry = LibraryRegistry::new(Arc::new(UnavailableLibraryStore));

        assert!(matches!(
            registry.register(&[registration("music", &["/a"])]).await,
            Err(LibraryRegistrationError::Store(
                TrackRepositoryError::Unavailable
            ))
        ));
    }

    #[tokio::test]
    async fn a_disabled_registry_reports_disabled() {
        let registry = LibraryRegistry::disabled();

        assert!(matches!(
            registry.libraries().await,
            Err(LibraryRegistrationError::Disabled)
        ));
    }

    #[test]
    fn a_label_token_rejects_whitespace_slashes_and_over_long_input() {
        assert!(is_label_token("music"));
        assert!(is_label_token("default"));
        assert!(!is_label_token(""));
        assert!(!is_label_token(" "));
        assert!(!is_label_token("mu sic"));
        assert!(!is_label_token("a/b"));
        assert!(!is_label_token(&"a".repeat(MAX_LABEL_LENGTH + 1)));
    }
}
