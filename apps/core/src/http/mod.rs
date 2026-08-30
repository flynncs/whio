mod errors;
mod middleware;
mod range;
mod raw_query;
mod routes;
mod subsonic;

use axum::{
    Router,
    routing::{get, post},
};

use crate::AppState;

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(routes::health::live))
        .route("/health/ready", get(routes::health::ready))
        .route("/catalogue/search", post(routes::catalogue::search))
        .route("/playback/resolve", post(routes::playback::resolve))
        .route(
            "/playback/tracks/{track_id}/stream",
            get(routes::playback::stream),
        )
        .nest("/rest", routes::rest::router())
        .fallback(routes::not_found)
        .layer(axum::middleware::from_fn(middleware::request_id))
        .with_state(state)
}
