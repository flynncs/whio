use std::path::PathBuf;

use async_trait::async_trait;

use crate::features::music_library::library::{Library, LibraryId};

use super::TrackRepositoryError;

#[async_trait]
pub trait LibraryStore: Send + Sync {
    async fn find_by_label(&self, label: &str) -> Result<Option<Library>, TrackRepositoryError>;

    async fn insert_library(
        &self,
        label: &str,
        roots: &[PathBuf],
    ) -> Result<Library, TrackRepositoryError>;

    async fn replace_roots(
        &self,
        library_id: &LibraryId,
        roots: &[PathBuf],
    ) -> Result<(), TrackRepositoryError>;

    async fn list_libraries(&self) -> Result<Vec<Library>, TrackRepositoryError>;

    async fn scopes_for_ids(&self, ids: &[LibraryId]) -> Result<Vec<String>, TrackRepositoryError>;
}
