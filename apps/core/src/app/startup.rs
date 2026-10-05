use std::{future::Future, sync::Arc};

use tokio::net::TcpListener;

use super::config::Config;
use crate::{
    app::AppState,
    app::database::Database,
    features::identity::{self, destinations::DestinationValidator},
    features::metadata::{METADATA_ADAPTER_VERSION, storage::PostgresMetadataStore},
    features::music_library::{
        self, ArtworkStore, DeliveryStore, EnrichmentGuard, PostgresTrackRepository,
        catalogue::CatalogueProvider, domain::YOUTUBE_MUSIC_PROVIDER_ID,
        library::LibraryRegistrationError,
    },
    features::playback::activity::PlaybackActivityService,
    features::playback::delivery::StreamDelivery,
    features::playback::media::{self, HttpMediaFetcher},
    features::playback::providers::{LocalPlaybackProvider, ProviderError},
    features::playback::{PlaybackProvider, StreamingService},
    integrations::{
        listenbrainz::ListenBrainzAdapter,
        ytm::{catalogue::YtmCatalogue, playback::YtmPlayback},
    },
};

pub(crate) async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl-C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler");

        signal.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {}
        _ = terminate => {}
    }

    tracing::info!("shutdown signal received");
}

pub async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;

    tracing_subscriber::fmt()
        .with_max_level(config.log_level)
        .init();

    let database = Arc::new(Database::connect(&config.database_url).await?);
    let _spool_directory =
        crate::features::playback::delivery::SpoolDirectory::open(&config.spool_dir).await?;
    let state = build_state(&config, database.clone())?;
    let listener = TcpListener::bind(config.bind_address).await?;

    tracing::info!(address = %config.bind_address, "listening");

    let scanner = state.scanner_handle();
    register_then_scan(state.library_registry().register(&config.libraries), || {
        scanner.start();
        scanner.trigger(crate::features::music_library::library::ScanMode::Incremental);
    })
    .await?;

    let media_roots = state.library_registry().scan_roots(&config.libraries);
    let _watcher = if media_roots.is_empty() {
        None
    } else {
        match crate::features::music_library::library::watch(&media_roots, Arc::clone(&scanner)) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                tracing::warn!(%error, "media watcher could not start");
                None
            }
        }
    };

    axum::serve(listener, crate::app::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn build_state(config: &Config, database: Arc<Database>) -> Result<AppState, ProviderError> {
    let transport = ytm_transport(config)?;
    let ytm_playback = Arc::new(YtmPlayback::new(transport.clone()));
    let ytm_catalogue = Arc::new(YtmCatalogue::new(transport.clone()));

    let credential = Arc::new(identity::credentials(
        database.pool(),
        config.credential_key.clone(),
    ));
    let (catalogue_providers, enabled_providers): (
        Vec<Arc<dyn CatalogueProvider>>,
        Vec<&'static str>,
    ) = if config.integrations.ytm.is_some() {
        (
            vec![ytm_catalogue as Arc<dyn CatalogueProvider>],
            vec![YOUTUBE_MUSIC_PROVIDER_ID],
        )
    } else {
        (Vec::new(), Vec::new())
    };

    let listenbrainz = Arc::new(
        crate::integrations::listenbrainz::ListenBrainzAdapter::new()
            .map_err(ProviderError::request)?,
    );
    let destinations = Arc::new(identity::destinations(
        database.pool(),
        Some(config.credential_key.clone()),
        destination_validator(&listenbrainz),
    ));

    let w4 = build_w4(
        config,
        database.pool(),
        &transport,
        &listenbrainz,
        &destinations,
    )?;

    let music = music_library::construct(music_library::LibraryWiring {
        pool: database.pool(),
        providers: catalogue_providers,
        enabled_providers,
        dedup_mode: config.dedup_mode,
        media_roots: config.media_roots(),
        artwork_host_suffixes: config.artwork_host_suffixes.clone(),
        w4,
    });

    let youtube_media_suffixes = vec![config.youtube_media_suffix.clone()];
    let fetcher = Arc::new(HttpMediaFetcher::with_allowed_host_suffixes(
        media::client(media::host_suffix_redirect_policy(
            youtube_media_suffixes.clone(),
        )),
        youtube_media_suffixes,
    ));
    let delivery = Arc::new(
        StreamDelivery::new(
            fetcher,
            config.spool_dir.clone(),
            config.max_playback_sessions,
        )
        .with_spool_limits(config.max_spool_bytes, config.spool_budget_bytes),
    );
    let local = Arc::new(LocalPlaybackProvider::new(Arc::clone(&music.sources)));
    let streaming = Arc::new(StreamingService::new(
        Arc::clone(&music.sources),
        vec![
            local as Arc<dyn PlaybackProvider>,
            ytm_playback as Arc<dyn PlaybackProvider>,
        ],
        delivery,
    ));

    let playback_activity = Arc::new(PlaybackActivityService::new(
        Arc::clone(&music.listen),
        Arc::clone(&music.library),
    ));
    let mut state =
        AppState::with_database(credential, music.search, music.library, streaming, database)
            .with_completion(music.completion)
            .with_playlists(music.playlists)
            .with_queue(music.queue)
            .with_listen(Arc::clone(&music.listen))
            .with_playback_activity(playback_activity)
            .with_artwork(music.artwork)
            .with_library_state(music.activity)
            .with_scanner(music.scanner)
            .with_library_registry(music.library_registry)
            .with_discovery(music.discovery)
            .with_metadata_defaults(
                config.metadata_artist_profile.clone(),
                config.metadata_language.clone(),
            )
            .with_destinations(destinations)
            .with_source_tags(config.source_tags);

    if let Some(delivery) = music.delivery.as_ref() {
        state = state.with_delivery(Arc::clone(delivery));
    }
    if let Some(metadata) = music.metadata.as_ref() {
        state = state.with_metadata_service(Arc::clone(metadata));
    }

    Ok(state)
}

pub(crate) struct WorkerState {
    pub(crate) metadata: Arc<crate::features::metadata::MetadataService>,
    pub(crate) subjects: Option<Arc<dyn crate::features::metadata::SubjectSource>>,
    pub(crate) delivery: Option<Arc<crate::features::music_library::delivery::DeliveryService>>,
}

pub(crate) fn worker_state(
    config: &Config,
    pool: sqlx::PgPool,
) -> Result<WorkerState, ProviderError> {
    let youtube_transport = ytm_transport(config)?;
    let listenbrainz = Arc::new(ListenBrainzAdapter::new().map_err(ProviderError::request)?);
    let destinations = Arc::new(identity::destinations(
        pool.clone(),
        Some(config.credential_key.clone()),
        destination_validator(&listenbrainz),
    ));
    let w4 = build_w4(
        config,
        pool.clone(),
        &youtube_transport,
        &listenbrainz,
        &destinations,
    )?;
    let music = music_library::construct(music_library::LibraryWiring {
        pool,
        providers: Vec::new(),
        enabled_providers: Vec::new(),
        dedup_mode: config.dedup_mode,
        media_roots: Vec::new(),
        artwork_host_suffixes: config.artwork_host_suffixes.clone(),
        w4,
    });

    Ok(WorkerState {
        metadata: music.metadata.ok_or_else(missing_metadata)?,
        subjects: Some(music.subjects),
        delivery: music.delivery,
    })
}

fn ytm_transport(
    config: &Config,
) -> Result<Option<Arc<whio_resolver_api::apis::configuration::Configuration>>, ProviderError> {
    config
        .integrations
        .ytm
        .as_ref()
        .map(|ytm| {
            crate::integrations::ytm::transport::resolver_client(
                ytm.url.clone(),
                ytm.connect_timeout,
                ytm.total_timeout,
            )
        })
        .transpose()
}

fn missing_metadata() -> ProviderError {
    ProviderError::request(std::io::Error::other(
        "the metadata service could not be constructed",
    ))
}

fn destination_validator(adapter: &Arc<ListenBrainzAdapter>) -> Arc<dyn DestinationValidator> {
    Arc::clone(adapter) as Arc<dyn DestinationValidator>
}

fn listen_sender(
    adapter: &Arc<ListenBrainzAdapter>,
) -> Arc<dyn crate::features::music_library::delivery::ListenSender> {
    Arc::clone(adapter) as Arc<dyn crate::features::music_library::delivery::ListenSender>
}

fn spawn_capability_warmer(capabilities: Arc<crate::features::metadata::CapabilitySnapshot>) {
    tokio::spawn(async move {
        let mut ticker =
            tokio::time::interval(crate::features::metadata::capabilities::CAPABILITY_TTL / 2);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            ticker.tick().await;

            if let Err(error) = capabilities.current(chrono::Utc::now()).await {
                tracing::debug!(%error, "the metadata capability advertisement is unavailable");
            }
        }
    });
}

fn build_w4(
    config: &Config,
    pool: sqlx::PgPool,
    youtube_transport: &Option<Arc<whio_resolver_api::apis::configuration::Configuration>>,
    listenbrainz: &Arc<ListenBrainzAdapter>,
    destinations: &Arc<identity::destinations::DestinationService>,
) -> Result<music_library::W4Dependencies, ProviderError> {
    let mut providers: Vec<Arc<dyn crate::features::metadata::DocumentProvider>> = Vec::new();
    if let Some(transport) = youtube_transport {
        providers.push(Arc::new(
            crate::integrations::ytm::documents::ResolverDocuments::new(
                crate::features::metadata::selection::YOUTUBE_MUSIC.to_owned(),
                Arc::clone(transport),
            ),
        ));
    }
    if let Some(config) = config.integrations.musicbrainz.as_ref() {
        let musicbrainz =
            crate::integrations::musicbrainz::artist::MusicBrainzDocuments::new(config)
                .map_err(ProviderError::request)?;
        for provider_id in [
            crate::features::metadata::selection::MUSICBRAINZ,
            crate::features::metadata::selection::WIKIPEDIA,
            crate::features::metadata::selection::COMMONS,
        ] {
            providers.push(Arc::new(musicbrainz.for_provider(provider_id)));
        }
    }
    if let Some(config) = config.integrations.audiodb.as_ref() {
        providers.push(Arc::new(
            crate::integrations::audiodb::AudioDbDocuments::new(config)
                .map_err(ProviderError::request)?,
        ));
    }
    if let Some(config) = config.integrations.listenbrainz_documents.as_ref() {
        providers.push(Arc::new(
            crate::integrations::listenbrainz::documents_artist::ListenBrainzDocuments::new(
                config,
                Arc::clone(listenbrainz.limiter()),
            )
            .map_err(ProviderError::request)?,
        ));
    }
    let documents = Arc::new(
        crate::features::metadata::DocumentProviders::new(providers)
            .map_err(ProviderError::request)?,
    );
    let capability_source: Arc<dyn crate::features::metadata::CapabilitySource> =
        Arc::clone(&documents) as Arc<dyn crate::features::metadata::CapabilitySource>;
    let capabilities = Arc::new(crate::features::metadata::CapabilitySnapshot::new(
        capability_source,
        crate::features::metadata::capabilities::CAPABILITY_TTL,
    ));
    spawn_capability_warmer(Arc::clone(&capabilities));

    let guard: Arc<dyn EnrichmentGuard> = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let metadata = {
        let store = Arc::new(PostgresMetadataStore::new(
            pool.clone(),
            Arc::clone(&guard),
            config.metadata_store_budget_bytes,
        ));

        Some(music_library::MetadataWiring {
            store,
            documents,
            capabilities,
            order: config.metadata_source_order.clone(),
            adapter_version: METADATA_ADAPTER_VERSION.to_owned(),
            jitter: Arc::new(crate::features::metadata::freshness::SystemJitter::new()),
        })
    };

    let artwork_store: Arc<PostgresTrackRepository> =
        Arc::new(PostgresTrackRepository::new(pool.clone()));
    let artwork_cache = Some(music_library::ArtworkCacheWiring {
        store: Arc::clone(&artwork_store) as Arc<dyn ArtworkStore>,
        root: config.artwork_cache_dir.clone(),
        budget_bytes: config.artwork_cache_bytes,
    });

    let delivery_store = Arc::new(PostgresTrackRepository::new(pool.clone()));
    let delivery_store: Arc<dyn DeliveryStore> = Arc::clone(&delivery_store) as Arc<_>;
    let delivery_source: Arc<dyn crate::features::music_library::delivery::ListenSource> =
        Arc::new(PostgresTrackRepository::new(pool.clone()));
    let sender: Arc<dyn crate::features::music_library::delivery::ListenSender> =
        listen_sender(listenbrainz);
    let delivery_destinations: Arc<
        dyn crate::features::music_library::delivery::DeliveryDestinations,
    > = Arc::new(IdentityDestinations::new(Arc::clone(destinations)));

    Ok(music_library::W4Dependencies {
        metadata,
        artwork_cache,
        delivery: Some(music_library::DeliveryWiring {
            store: delivery_store,
            source: delivery_source,
            sender,
            destinations: delivery_destinations,
        }),
    })
}

struct IdentityDestinations {
    destinations: Arc<identity::destinations::DestinationService>,
}

impl IdentityDestinations {
    fn new(destinations: Arc<identity::destinations::DestinationService>) -> Self {
        Self { destinations }
    }
}

#[async_trait::async_trait]
impl crate::features::music_library::delivery::DeliveryDestinations for IdentityDestinations {
    async fn enabled_destinations(
        &self,
        user_id: uuid::Uuid,
    ) -> Result<Vec<(uuid::Uuid, String)>, crate::features::music_library::delivery::DeliveryError>
    {
        self.destinations
            .enabled_destinations(user_id)
            .await
            .map_err(destination_failure)
    }

    async fn enabled_credential(
        &self,
        user_id: uuid::Uuid,
        provider_id: &str,
    ) -> Result<
        Option<identity::destinations::DestinationCredential>,
        crate::features::music_library::delivery::DeliveryError,
    > {
        self.destinations
            .enabled_credential(user_id, provider_id)
            .await
            .map_err(destination_failure)
    }
}

fn destination_failure(
    error: identity::destinations::DestinationError,
) -> crate::features::music_library::delivery::DeliveryError {
    use crate::features::music_library::delivery::DeliveryError;

    match error {
        identity::destinations::DestinationError::Disabled => DeliveryError::Unavailable,
        identity::destinations::DestinationError::Unavailable => DeliveryError::ProviderUnavailable,
        identity::destinations::DestinationError::InvalidCredential => {
            DeliveryError::CredentialRejected
        }
        identity::destinations::DestinationError::Repository(_)
        | identity::destinations::DestinationError::NotFound
        | identity::destinations::DestinationError::AlreadyConnected
        | identity::destinations::DestinationError::Unsupported => DeliveryError::Disabled,
    }
}

async fn register_then_scan<T>(
    registration: impl Future<Output = Result<T, LibraryRegistrationError>>,
    start_scan: impl FnOnce(),
) -> Result<(), LibraryRegistrationError> {
    registration.await?;
    start_scan();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::metadata::{CapabilityError, CapabilitySource, Snapshot};
    use crate::features::music_library::test_support::TrackRepositoryError;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct CountingCapabilities(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl CapabilitySource for CountingCapabilities {
        async fn fetch(&self) -> Result<Snapshot, CapabilityError> {
            self.0.fetch_add(1, Ordering::SeqCst);

            Ok(Snapshot::new(Vec::new()))
        }
    }

    #[tokio::test]
    async fn the_capability_warmer_holds_an_advertisement_before_any_read() {
        let calls = Arc::new(AtomicUsize::new(0));
        let snapshot = Arc::new(crate::features::metadata::CapabilitySnapshot::new(
            Arc::new(CountingCapabilities(Arc::clone(&calls))),
            crate::features::metadata::capabilities::CAPABILITY_TTL,
        ));

        spawn_capability_warmer(Arc::clone(&snapshot));

        for _ in 0..10_000 {
            if calls.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::task::yield_now().await;
        }

        assert!(calls.load(Ordering::SeqCst) > 0);
        assert!(snapshot.held(chrono::Utc::now()).is_some());
    }

    #[tokio::test]
    async fn a_failed_registration_stops_startup_before_the_scanner_starts() {
        let started = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&started);

        let result = register_then_scan(
            async {
                Err::<Vec<crate::features::music_library::library::Library>, _>(
                    LibraryRegistrationError::Store(TrackRepositoryError::Unavailable),
                )
            },
            move || flag.store(true, Ordering::SeqCst),
        )
        .await;

        assert!(matches!(
            result,
            Err(LibraryRegistrationError::Store(
                TrackRepositoryError::Unavailable
            ))
        ));
        assert!(!started.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_successful_registration_starts_the_scanner_afterwards() {
        let started = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&started);

        let result =
            register_then_scan(async { Ok(()) }, move || flag.store(true, Ordering::SeqCst)).await;

        assert!(result.is_ok());
        assert!(started.load(Ordering::SeqCst));
    }

    #[test]
    fn the_destination_validator_and_listen_sender_share_one_adapter_allocation() {
        let adapter = Arc::new(ListenBrainzAdapter::new().unwrap());
        let validator = destination_validator(&adapter);
        let sender = listen_sender(&adapter);

        assert_eq!(Arc::strong_count(&adapter), 3);
        assert_eq!(
            Arc::as_ptr(&validator) as *const (),
            Arc::as_ptr(&sender) as *const ()
        );
        assert!(Arc::ptr_eq(adapter.limiter(), adapter.limiter()));
    }

    #[cfg(feature = "postgres-integration")]
    #[sqlx::test]
    async fn the_startup_sequence_registers_before_a_real_scan(pool: sqlx::PgPool) {
        use crate::features::music_library::{
            library::{LibraryRegistration, LibraryRegistry, ScanMode, ScanService},
            test_support::{
                CatalogueWrite, LibraryStore, PostgresLibraryStore, PostgresTrackRepository,
                SourceLookup,
            },
        };

        let migrated: bool = sqlx::query_scalar("SELECT to_regclass('libraries') IS NOT NULL")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(migrated);
        let registered_before: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM libraries")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(registered_before, 0);

        let root = std::env::temp_dir().join(format!("whio-startup-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tagged.flac"),
            root.join("a.flac"),
        )
        .unwrap();

        let registrations = [LibraryRegistration::new(
            "music".to_owned(),
            vec![root.clone()],
        )];
        let library_store: Arc<dyn LibraryStore> =
            Arc::new(PostgresLibraryStore::new(pool.clone()));
        let registry = LibraryRegistry::new(library_store);
        let tracks = Arc::new(PostgresTrackRepository::new(pool.clone()));
        let scanner = ScanService::new(
            Arc::clone(&tracks) as Arc<dyn CatalogueWrite>,
            Arc::clone(&tracks) as Arc<dyn SourceLookup>,
            registry.scan_roots(&registrations),
        );

        register_then_scan(registry.register(&registrations), || {
            scanner.start();
            scanner.trigger(ScanMode::Incremental);
        })
        .await
        .unwrap();

        let libraries = registry.libraries().await.unwrap();
        assert_eq!(libraries.len(), 1);
        assert_eq!(libraries[0].label(), "music");
        assert_eq!(libraries[0].numeric_id(), 1);

        let mut report = None;
        for _ in 0..1_000_000 {
            if let Some(last) = scanner.status().last_report {
                report = Some(last);
                break;
            }
            tokio::task::yield_now().await;
        }
        let report = report.expect("the started scanner completed a scan");
        assert_eq!(report.indexed, 1);

        let recorded: String = sqlx::query_scalar(
            "SELECT source_scope FROM track_sources WHERE provider_id = 'local'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(recorded, format!("named:{}", root.display()));
        assert_eq!(registry.scopes_for("music").await.unwrap(), vec![recorded]);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
