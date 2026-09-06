pub(crate) mod errors;
mod middleware;
mod range;
pub(crate) mod raw_query;
pub(crate) mod routes;

use axum::{
    Router,
    routing::{get, post},
};

use crate::{AppState, compat};
use axum::middleware::from_fn;

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
        .nest("/rest", compat::subsonic::router())
        .nest(
            "/compat/subsonic/rest",
            compat::subsonic::router().layer(from_fn(compat::cors)),
        )
        .nest(
            "/compat/navidrome/rest",
            compat::subsonic::router().layer(from_fn(compat::cors)),
        )
        .nest(
            "/compat/navidrome",
            compat::navidrome::router().layer(from_fn(compat::cors)),
        )
        .fallback(routes::not_found)
        .layer(axum::middleware::from_fn(middleware::request_id))
        .with_state(state)
}
