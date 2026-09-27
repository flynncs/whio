use std::sync::Arc;

use crate::{
    facades::SourceTags,
    features::{
        identity::service::CredentialService,
        music_library::{
            artwork::ArtworkService,
            catalogue::{CompletionService, LibraryService, SearchService},
            library::{LibraryRegistry, ScanService},
            library_state::LibraryStateService,
            playlists::PlaylistService,
            queue::RestoreQueueService,
        },
        playback::StreamingService,
    },
};

/// Only public application operations needed by the Subsonic protocol.
#[derive(Clone)]
pub(crate) struct SubsonicState {
    pub(crate) credential: Arc<CredentialService>,
    pub(crate) search: Arc<SearchService>,
    pub(crate) library: Arc<LibraryService>,
    pub(crate) libraries: Arc<LibraryRegistry>,
    pub(crate) completion: Arc<CompletionService>,
    pub(crate) playlists: Arc<PlaylistService>,
    pub(crate) queue: Arc<RestoreQueueService>,
    pub(crate) artwork: Arc<ArtworkService>,
    pub(crate) library_state: Arc<LibraryStateService>,
    pub(crate) scanner: Arc<ScanService>,
    pub(crate) streaming: Arc<StreamingService>,
    pub(crate) source_tags: SourceTags,
}

impl SubsonicState {
    pub(crate) fn credential(&self) -> &CredentialService {
        &self.credential
    }
    pub(crate) fn search(&self) -> &SearchService {
        &self.search
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
    pub(crate) fn queue(&self) -> &RestoreQueueService {
        &self.queue
    }
    pub(crate) fn artwork(&self) -> &ArtworkService {
        &self.artwork
    }
    pub(crate) fn library_state(&self) -> &LibraryStateService {
        &self.library_state
    }
    pub(crate) fn scanner(&self) -> &ScanService {
        &self.scanner
    }
    pub(crate) fn streaming(&self) -> &StreamingService {
        &self.streaming
    }
    pub(crate) fn source_tags(&self) -> SourceTags {
        self.source_tags
    }
}
