use std::sync::Arc;

use crate::{
    facades::SourceTags,
    features::{
        identity::service::CredentialService,
        music_library::{
            catalogue::{CompletionService, LibraryService, SearchService},
            playlists::PlaylistService,
        },
    },
};

#[derive(Clone)]
pub(crate) struct NavidromeState {
    pub(crate) credential: Arc<CredentialService>,
    pub(crate) search: Arc<SearchService>,
    pub(crate) library: Arc<LibraryService>,
    pub(crate) completion: Arc<CompletionService>,
    pub(crate) playlists: Arc<PlaylistService>,
    pub(crate) source_tags: SourceTags,
}

impl NavidromeState {
    pub(crate) fn credential(&self) -> &CredentialService {
        &self.credential
    }
    pub(crate) fn search(&self) -> &SearchService {
        &self.search
    }
    pub(crate) fn library(&self) -> &LibraryService {
        &self.library
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
