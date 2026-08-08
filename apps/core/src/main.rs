use std::sync::Arc;

use tokio::net::TcpListener;

mod config;

use config::Config;
use whio_core::{
    AppState,
    catalogue::CatalogueService,
    resolver::ResolverClient,
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

    let listener = TcpListener::bind(config.bind_address).await?;

    tracing::info!(address = %config.bind_address, "listening");

    let track_repository: Arc<dyn TrackRepository> = Arc::new(InMemoryTrackRepository::default());

    let resolver = ResolverClient::new(
        config.resolver_url,
        config.resolver_connect_timeout,
        config.resolver_total_timeout,
    )?;
    let catalogue = Arc::new(CatalogueService::new(Arc::new(resolver), track_repository));
    let state = AppState::new(catalogue);

    axum::serve(listener, whio_core::router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    Ok(())
}
