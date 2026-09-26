use std::sync::Arc;

use crate::{
    facades::SourceTags,
    features::{
        identity::service::CredentialService,
        music_library::{
            catalogue::{CompletionService, LibraryService},
            library::LibraryRegistry,
            playlists::PlaylistService,
        },
    },
};

#[derive(Clone)]
pub(crate) struct NavidromeState {
    pub(crate) credential: Arc<CredentialService>,
    pub(crate) library: Arc<LibraryService>,
    pub(crate) libraries: Arc<LibraryRegistry>,
    pub(crate) completion: Arc<CompletionService>,
    pub(crate) playlists: Arc<PlaylistService>,
    pub(crate) source_tags: SourceTags,
}

impl NavidromeState {
    pub(crate) fn credential(&self) -> &CredentialService {
        &self.credential
    }
    pub(crate) fn library(&self) -> &LibraryService {
        &self.library
    }
    pub(crate) fn libraries(&self) -> &LibraryRegistry {
        &self.libraries
    }
    pub(crate) fn completion(&self) -> &CompletionService {
        &self.completion
    }
    pub(crate) fn playlists(&self) -> &PlaylistService {
        &self.playlists
    }
    pub(crate) fn source_tags(&self) -> SourceTags {
        self.source_tags
    }
}
