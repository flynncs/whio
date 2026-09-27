use std::{future::Future, sync::Arc};

use tokio::net::TcpListener;

use super::config::{Config, YoutubeResolverConfig};
use crate::{
    app::AppState,
    app::{database::Database, resolver},
    features::identity,
    features::music_library::{
        self, catalogue::CatalogueProvider, domain::YOUTUBE_MUSIC_PROVIDER_ID,
        library::LibraryRegistrationError, providers::YoutubeCatalogueProvider,
    },
    features::playback::delivery::StreamDelivery,
    features::playback::media::{self, HttpMediaFetcher},
    features::playback::providers::{LocalPlaybackProvider, ProviderError, YtmAdapter},
    features::playback::{PlaybackProvider, StreamingService},
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
    let transport = match &config.youtube_resolver {
        YoutubeResolverConfig::Disabled => None,
        YoutubeResolverConfig::Enabled {
            url,
            connect_timeout,
            total_timeout,
        } => Some(resolver::client(
            url.clone(),
            *connect_timeout,
            *total_timeout,
        )?),
    };
    let ytm_playback = Arc::new(YtmAdapter::new(transport.clone()));
    let ytm_catalogue = Arc::new(YoutubeCatalogueProvider::new(transport));

    let credential = Arc::new(identity::credentials(
        database.pool(),
        config.credential_key.clone(),
    ));
    let (catalogue_providers, enabled_providers): (
        Vec<Arc<dyn CatalogueProvider>>,
        Vec<&'static str>,
    ) = match &config.youtube_resolver {
        YoutubeResolverConfig::Disabled => (Vec::new(), Vec::new()),
        YoutubeResolverConfig::Enabled { .. } => (
            vec![ytm_catalogue as Arc<dyn CatalogueProvider>],
            vec![YOUTUBE_MUSIC_PROVIDER_ID],
        ),
    };
    let music = music_library::construct(
        database.pool(),
        catalogue_providers,
        enabled_providers,
        config.dedup_mode,
        config.media_roots(),
        config.artwork_host_suffixes.clone(),
    );
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

    Ok(
        AppState::with_database(credential, music.search, music.library, streaming, database)
            .with_completion(music.completion)
            .with_playlists(music.playlists)
            .with_queue(music.queue)
            .with_artwork(music.artwork)
            .with_library_state(music.activity)
            .with_scanner(music.scanner)
            .with_library_registry(music.library_registry)
            .with_source_tags(config.source_tags),
    )
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
    use crate::features::music_library::test_support::TrackRepositoryError;
    use std::sync::atomic::{AtomicBool, Ordering};

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
