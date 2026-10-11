use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;

use crate::app::config::Config;
use crate::features::acquisition::ports::{
    AcquisitionTarget, CandidateSearch, LibraryOwnership, MusicBrainzLookup, OwnershipError,
    RemoteTransfer,
};
use crate::features::acquisition::service::{AcquisitionPorts, AcquisitionService};
use crate::features::acquisition::storage::PostgresAcquisitionStore;
use crate::features::music_library::TrackRepositoryError;
use crate::features::music_library::catalogue::{
    CatalogueError, CompletionService, LibraryService,
};
use crate::features::playback::providers::ProviderError;
use crate::integrations::beets::Beets;
use crate::integrations::musicbrainz::client::MusicBrainzClient;
use crate::integrations::slskd::Slskd;
use crate::platform::clock::Clock;

struct OwnershipLibrary {
    library: Arc<LibraryService>,
}

impl OwnershipLibrary {
    fn new(library: Arc<LibraryService>) -> Self {
        Self { library }
    }
}

#[async_trait]
impl LibraryOwnership for OwnershipLibrary {
    #[tracing::instrument(skip_all, fields(kind = target.kind()))]
    async fn owns(&self, target: &AcquisitionTarget) -> Result<bool, OwnershipError> {
        let owned = match target {
            AcquisitionTarget::Release(id) => self.library.owns_release(&id.to_string()).await,
            AcquisitionTarget::Recording(id) => self.library.owns_recording(&id.to_string()).await,
        };

        owned.map_err(map_ownership)
    }
}

fn map_ownership(error: CatalogueError) -> OwnershipError {
    match error {
        CatalogueError::Repository(TrackRepositoryError::Unavailable) => {
            OwnershipError::Unavailable
        }
        CatalogueError::Provider(
            crate::features::music_library::catalogue::ProviderError::Disabled
            | crate::features::music_library::catalogue::ProviderError::ProviderUnavailable
            | crate::features::music_library::catalogue::ProviderError::Request(_),
        ) => OwnershipError::Unavailable,
        CatalogueError::Invalid(_)
        | CatalogueError::Provider(_)
        | CatalogueError::Repository(_) => OwnershipError::Failed,
    }
}

pub(crate) fn build(
    config: &Config,
    pool: PgPool,
    library: Arc<LibraryService>,
    completion: Arc<CompletionService>,
    musicbrainz: Option<&Arc<MusicBrainzClient>>,
) -> Result<Option<Arc<AcquisitionService>>, ProviderError> {
    if !config.acquisition.enabled {
        return Ok(None);
    }

    let acquisition = &config.acquisition;
    let slskd = config
        .integrations
        .slskd
        .as_ref()
        .ok_or_else(|| missing("WHIO_SLSKD_ENABLED"))?;
    let beets = config
        .integrations
        .beets
        .as_ref()
        .ok_or_else(|| missing("WHIO_BEETS_COMMAND"))?;
    let lookup = musicbrainz
        .ok_or_else(|| missing("WHIO_MUSICBRAINZ_ENABLED"))
        .map(Arc::clone)?;
    let library_dir = acquisition
        .library_dir()
        .ok_or_else(|| missing("WHIO_ACQUISITION_LIBRARY_DIR"))?
        .to_path_buf();

    let slskd = Arc::new(Slskd::new(slskd).map_err(ProviderError::request)?);
    let search: Arc<dyn CandidateSearch> = Arc::clone(&slskd) as Arc<dyn CandidateSearch>;
    let transfer: Arc<dyn RemoteTransfer> = Arc::clone(&slskd) as Arc<dyn RemoteTransfer>;
    let lookup: Arc<dyn MusicBrainzLookup> = lookup as Arc<dyn MusicBrainzLookup>;
    let importer = Arc::new(Beets::new(beets, library_dir));
    let ownership = Arc::new(OwnershipLibrary::new(Arc::clone(&library)));
    let ports = AcquisitionPorts::new(search, transfer, lookup, importer, ownership, completion);

    Ok(Some(Arc::new(AcquisitionService::new(
        Arc::new(PostgresAcquisitionStore::new(pool)),
        ports,
        library,
        acquisition.clone(),
        Clock::system(),
    ))))
}

fn missing(name: &'static str) -> ProviderError {
    ProviderError::request(std::io::Error::other(format!(
        "acquisition requires {name}"
    )))
}
