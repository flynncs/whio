use std::sync::Arc;

use tokio::net::TcpListener;

mod config;

use config::{Config, YoutubeResolverConfig};
use whio_core::{
    AppState,
    catalogue::{CatalogueResolver, CatalogueService},
    playback::{PlaybackResolver, PlaybackService},
    resolver::{DisabledResolver, ResolverClient, ResolverError},
    tracks::{InMemoryTrackRepository, TrackRepository},
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

    let state = build_state(&config)?;
    let listener = TcpListener::bind(config.bind_address).await?;

    tracing::info!(address = %config.bind_address, "listening");

    axum::serve(listener, whio_core::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}

fn build_state(config: &Config) -> Result<AppState, ResolverError> {
    let track_repository: Arc<dyn TrackRepository> = Arc::new(InMemoryTrackRepository::default());

    let (catalogue_resolver, playback_resolver): (
        Arc<dyn CatalogueResolver>,
        Arc<dyn PlaybackResolver>,
    ) = match &config.youtube_resolver {
        YoutubeResolverConfig::Disabled => {
            let resolver = Arc::new(DisabledResolver);
            (
                resolver.clone() as Arc<dyn CatalogueResolver>,
                resolver as Arc<dyn PlaybackResolver>,
            )
        }
        YoutubeResolverConfig::Enabled {
            url,
            connect_timeout,
            total_timeout,
        } => {
            let resolver = Arc::new(ResolverClient::new(
                url.clone(),
                *connect_timeout,
                *total_timeout,
            )?);
            (
                resolver.clone() as Arc<dyn CatalogueResolver>,
                resolver as Arc<dyn PlaybackResolver>,
            )
        }
    };

    let catalogue = Arc::new(CatalogueService::new(
        catalogue_resolver,
        track_repository.clone(),
    ));
    let playback = Arc::new(PlaybackService::new(playback_resolver, track_repository));

    Ok(AppState::new(catalogue, playback))
}
