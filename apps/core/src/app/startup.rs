use std::sync::Arc;

use tokio::net::TcpListener;

use super::config::{Config, YoutubeResolverConfig};
use crate::{
    app::AppState,
    app::{database::Database, resolver},
    features::identity,
    features::music_library::{
        self, catalogue::CatalogueProvider, domain::YOUTUBE_MUSIC_PROVIDER_ID,
        providers::YoutubeCatalogueProvider,
    },
    features::playback::delivery::StreamDelivery,
    features::playback::media::{self, HttpMediaFetcher},
    features::playback::providers::{LocalPlaybackProvider, ProviderError, YtmAdapter},
    features::playback::{PlaybackProvider, StreamingService},
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
    scanner.start();
    scanner.trigger(crate::features::music_library::library::ScanMode::Incremental);

    let _watcher = if config.media_roots.is_empty() {
        None
    } else {
        match crate::features::music_library::library::watch(
            &config.media_roots,
            Arc::clone(&scanner),
        ) {
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
        config.media_roots.clone(),
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
            .with_artwork(music.artwork)
            .with_library_state(music.activity)
            .with_scanner(music.scanner)
            .with_source_tags(config.source_tags),
    )
}
