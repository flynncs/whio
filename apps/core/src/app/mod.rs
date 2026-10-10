pub mod cli;
mod config;
mod startup;
pub use startup::run;

pub mod database;
pub(crate) mod http;
pub mod worker;

use std::sync::Arc;

use axum::{Router, extract::FromRef};

use crate::{
    facades::SourceTags,
    features::identity::destinations::DestinationService,
    features::identity::service::CredentialService,
    features::metadata::MetadataService,
    features::music_library::artwork::ArtworkService,
    features::music_library::catalogue::{CompletionService, LibraryService, SearchService},
    features::music_library::delivery::DeliveryService,
    features::music_library::discovery::DiscoveryService,
    features::music_library::history::ListenService,
    features::music_library::library::{LibraryRegistry, ScanService},
    features::music_library::library_state::LibraryStateService,
    features::music_library::playlists::PlaylistService,
    features::music_library::queue::RestoreQueueService,
    features::playback::StreamingService,
    features::playback::activity::PlaybackActivityService,
    platform::client_ip::ClientIpPolicy,
};

#[derive(Clone)]
pub struct AppState {
    artwork: Arc<ArtworkService>,
    search: Arc<SearchService>,
    library: Arc<LibraryService>,
    completion: Arc<CompletionService>,
    playlists: Arc<PlaylistService>,
    queue: Arc<RestoreQueueService>,
    listen: Arc<ListenService>,
    playback_activity: Arc<PlaybackActivityService>,
    credential: Arc<CredentialService>,
    database: Option<Arc<database::Database>>,
    library_state: Arc<LibraryStateService>,
    streaming: Arc<StreamingService>,
    scanner: Arc<ScanService>,
    library_registry: Arc<LibraryRegistry>,
    discovery: Option<Arc<DiscoveryService>>,
    destinations: Option<Arc<DestinationService>>,
    delivery: Option<Arc<DeliveryService>>,
    metadata: Option<Arc<MetadataService>>,
    metadata_profile: crate::features::metadata::MetadataProfile,
    metadata_language: String,
    source_tags: SourceTags,
    pub(crate) client_ip_policy: Arc<ClientIpPolicy>,
}

impl AppState {
    pub fn new(
        credential: Arc<CredentialService>,
        search: Arc<SearchService>,
        library: Arc<LibraryService>,
        streaming: Arc<StreamingService>,
    ) -> Self {
        Self::build(credential, search, library, streaming, None)
    }

    pub fn with_database(
        credential: Arc<CredentialService>,
        search: Arc<SearchService>,
        library: Arc<LibraryService>,
        streaming: Arc<StreamingService>,
        database: Arc<database::Database>,
    ) -> Self {
        Self::build(credential, search, library, streaming, Some(database))
    }

    pub fn with_client_ip_policy(mut self, policy: ClientIpPolicy) -> Self {
        self.client_ip_policy = Arc::new(policy);
        self
    }

    pub fn with_artwork(mut self, artwork: Arc<ArtworkService>) -> Self {
        self.artwork = artwork;
        self
    }

    pub fn with_library_state(mut self, library_state: Arc<LibraryStateService>) -> Self {
        self.library_state = library_state;
        self
    }

    pub fn with_completion(mut self, completion: Arc<CompletionService>) -> Self {
        self.completion = completion;
        self
    }

    pub fn with_queue(mut self, queue: Arc<RestoreQueueService>) -> Self {
        self.queue = queue;
        self
    }

    pub fn with_listen(mut self, listen: Arc<ListenService>) -> Self {
        self.listen = listen;
        self
    }

    pub fn with_playback_activity(mut self, activity: Arc<PlaybackActivityService>) -> Self {
        self.playback_activity = activity;
        self
    }

    pub fn with_playlists(mut self, playlists: Arc<PlaylistService>) -> Self {
        self.playlists = playlists;
        self
    }

    pub fn with_discovery(mut self, discovery: Arc<DiscoveryService>) -> Self {
        self.discovery = Some(discovery);
        self
    }

    pub fn with_destinations(mut self, destinations: Arc<DestinationService>) -> Self {
        self.destinations = Some(destinations);
        self
    }

    pub fn with_delivery(mut self, delivery: Arc<DeliveryService>) -> Self {
        self.delivery = Some(delivery);
        self
    }

    pub fn with_metadata_service(mut self, metadata: Arc<MetadataService>) -> Self {
        self.metadata = Some(metadata);
        self
    }

    pub fn with_metadata_defaults(
        mut self,
        profile: crate::features::metadata::MetadataProfile,
        language: String,
    ) -> Self {
        self.metadata_profile = profile;
        self.metadata_language = language;
        self
    }

    pub fn metadata_profile(&self) -> &crate::features::metadata::MetadataProfile {
        &self.metadata_profile
    }

    pub fn metadata_language(&self) -> &str {
        &self.metadata_language
    }

    pub fn with_source_tags(mut self, tags: SourceTags) -> Self {
        self.source_tags = tags;
        self
    }

    pub fn with_scanner(mut self, scanner: Arc<ScanService>) -> Self {
        self.scanner = scanner;
        self
    }

    pub fn with_library_registry(mut self, library_registry: Arc<LibraryRegistry>) -> Self {
        self.library_registry = library_registry;
        self
    }

    pub(crate) fn library_registry(&self) -> &Arc<LibraryRegistry> {
        &self.library_registry
    }

    pub fn scanner_handle(&self) -> Arc<ScanService> {
        Arc::clone(&self.scanner)
    }

    pub fn delivery(&self) -> Option<Arc<DeliveryService>> {
        self.delivery.clone()
    }

    pub fn destinations(&self) -> Option<Arc<DestinationService>> {
        self.destinations.clone()
    }

    pub fn metadata_service(&self) -> Option<Arc<MetadataService>> {
        self.metadata.clone()
    }

    pub fn discovery_service(&self) -> Option<Arc<DiscoveryService>> {
        self.discovery.clone()
    }

    fn build(
        credential: Arc<CredentialService>,
        search: Arc<SearchService>,
        library: Arc<LibraryService>,
        streaming: Arc<StreamingService>,
        database: Option<Arc<database::Database>>,
    ) -> Self {
        let completion = Arc::new(CompletionService::disabled(
            Arc::clone(&library),
            Arc::clone(&search),
        ));
        let playlists = Arc::new(PlaylistService::disabled(Arc::clone(&library)));
        let queue = Arc::new(RestoreQueueService::disabled());
        let listen = Arc::new(ListenService::disabled());
        let playback_activity = Arc::new(PlaybackActivityService::disabled(Arc::clone(&library)));

        Self {
            artwork: Arc::new(ArtworkService::disabled()),
            credential,
            search,
            library,
            completion,
            playlists,
            queue,
            listen,
            playback_activity,
            database,
            library_state: Arc::new(LibraryStateService::disabled()),
            streaming,
            scanner: ScanService::disabled(),
            library_registry: Arc::new(LibraryRegistry::disabled()),
            discovery: None,
            destinations: None,
            delivery: None,
            metadata: None,
            metadata_profile: crate::features::metadata::MetadataProfile::artist_display(),
            metadata_language: "en".to_owned(),
            source_tags: SourceTags::Off,
            client_ip_policy: Arc::new(ClientIpPolicy {
                trusted_proxies: Vec::new(),
            }),
        }
    }

    pub(crate) fn database(&self) -> Option<&database::Database> {
        self.database.as_deref()
    }
}

impl FromRef<AppState> for crate::features::music_library::api::native::NativeState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            search: Arc::clone(&state.search),
            library: Arc::clone(&state.library),
            artwork: Arc::clone(&state.artwork),
        }
    }
}

impl FromRef<AppState> for crate::features::music_library::api::playlists::PlaylistsState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            playlists: Arc::clone(&state.playlists),
            library: Arc::clone(&state.library),
        }
    }
}

impl FromRef<AppState> for crate::features::music_library::api::queue::QueueState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            queue: Arc::clone(&state.queue),
            library: Arc::clone(&state.library),
        }
    }
}

impl FromRef<AppState> for crate::features::music_library::api::history::HistoryState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            listen: Arc::clone(&state.listen),
            playback_activity: Arc::clone(&state.playback_activity),
        }
    }
}

impl FromRef<AppState> for crate::features::playback::api::native::NativeState {
    fn from_ref(state: &AppState) -> Self {
        Self(Arc::clone(&state.streaming))
    }
}

impl FromRef<AppState> for crate::features::identity::api::native::NativeState {
    fn from_ref(state: &AppState) -> Self {
        Self(Arc::clone(&state.credential))
    }
}

impl FromRef<AppState> for crate::facades::subsonic::state::SubsonicState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            credential: Arc::clone(&state.credential),
            search: Arc::clone(&state.search),
            library: Arc::clone(&state.library),
            libraries: Arc::clone(&state.library_registry),
            completion: Arc::clone(&state.completion),
            playlists: Arc::clone(&state.playlists),
            queue: Arc::clone(&state.queue),
            artwork: Arc::clone(&state.artwork),
            library_state: Arc::clone(&state.library_state),
            activity: Arc::clone(&state.playback_activity),
            scanner: Arc::clone(&state.scanner),
            streaming: Arc::clone(&state.streaming),
            discovery: state.discovery.clone(),
            source_tags: state.source_tags,
        }
    }
}

impl FromRef<AppState> for crate::facades::navidrome::state::NavidromeState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            credential: Arc::clone(&state.credential),
            library: Arc::clone(&state.library),
            libraries: Arc::clone(&state.library_registry),
            completion: Arc::clone(&state.completion),
            playlists: Arc::clone(&state.playlists),
            source_tags: state.source_tags,
        }
    }
}

impl FromRef<AppState> for crate::facades::navidrome::state::NavidromeQueueState {
    fn from_ref(state: &AppState) -> Self {
        Self {
            navidrome:
                <crate::facades::navidrome::state::NavidromeState as FromRef<AppState>>::from_ref(
                    state,
                ),
            queue: Arc::clone(&state.queue),
        }
    }
}

pub fn router(state: AppState) -> Router {
    http::router(state)
}
