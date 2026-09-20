use std::sync::Arc;

use tokio::net::TcpListener;

mod config;

use config::{Config, YoutubeResolverConfig};
use whio_core::{
    AppState,
    catalogue::{CatalogueProvider, CatalogueService},
    database::Database,
    delivery::StreamDelivery,
    identity::{
        postgres::PostgresCredentialRepository, repository::CredentialRepository,
        service::CredentialService,
    },
    media::{self, HttpMediaFetcher},
    playback::{PlaybackProvider, StreamingService},
    providers::{LocalPlaybackProvider, ProviderError, YtmAdapter},
    tracks::{PostgresTrackRepository, TrackRepository},
};

async fn shutdown_signal() {
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

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let config = Config::from_env()?;

    tracing_subscriber::fmt()
        .with_max_level(config.log_level)
        .init();

    let database = Arc::new(Database::connect(&config.database_url).await?);
    let _spool_directory = whio_core::delivery::SpoolDirectory::open(&config.spool_dir).await?;
    let state = build_state(&config, database.clone())?;
    let listener = TcpListener::bind(config.bind_address).await?;

    tracing::info!(address = %config.bind_address, "listening");

    let scanner = state.scanner_handle();
    scanner.start();
    scanner.trigger(whio_core::library::ScanMode::Incremental);

    let _watcher = if config.media_roots.is_empty() {
        None
    } else {
        match whio_core::library::watch(&config.media_roots, Arc::clone(&scanner)) {
            Ok(watcher) => Some(watcher),
            Err(error) => {
                tracing::warn!(%error, "media watcher could not start");
                None
            }
        }
    };

    axum::serve(listener, whio_core::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn build_state(config: &Config, database: Arc<Database>) -> Result<AppState, ProviderError> {
    let track_repository: Arc<dyn TrackRepository> =
        Arc::new(PostgresTrackRepository::new(database.pool()));
    let credential_repository: Arc<dyn CredentialRepository> =
        Arc::new(PostgresCredentialRepository::new(database.pool()));

    let ytm = Arc::new(match &config.youtube_resolver {
        YoutubeResolverConfig::Disabled => YtmAdapter::disabled(),
        YoutubeResolverConfig::Enabled {
            url,
            connect_timeout,
            total_timeout,
        } => YtmAdapter::new(url.clone(), *connect_timeout, *total_timeout)?,
    });

    let credential = Arc::new(CredentialService::new(
        credential_repository,
        config.credential_key.clone(),
    ));
    let catalogue = Arc::new(CatalogueService::with_providers(
        track_repository.clone(),
        vec![Arc::clone(&ytm) as Arc<dyn CatalogueProvider>],
    ));
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
    let local = Arc::new(LocalPlaybackProvider::new(track_repository.clone()));
    let scanner =
        whio_core::library::ScanService::new(track_repository.clone(), config.media_roots.clone());
    let artwork = Arc::new(whio_core::artwork::ArtworkService::new(
        track_repository.clone(),
        config.artwork_host_suffixes.clone(),
    ));
    let library_state = Arc::new(whio_core::library_state::LibraryStateService::new(
        track_repository.clone(),
    ));
    let streaming = Arc::new(StreamingService::new(
        track_repository,
        vec![
            local as Arc<dyn PlaybackProvider>,
            ytm as Arc<dyn PlaybackProvider>,
        ],
        delivery,
    ));

    Ok(
        AppState::with_database(credential, catalogue, streaming, database)
            .with_artwork(artwork)
            .with_library_state(library_state)
            .with_scanner(scanner)
            .with_source_tags(config.source_tags),
    )
}
