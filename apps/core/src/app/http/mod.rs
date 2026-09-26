mod middleware;
pub(crate) mod routes;

use axum::{
    Router,
    routing::{get, post},
};

use crate::{
    app::AppState,
    facades,
    features::{music_library, playback},
};
use axum::middleware::from_fn;

pub(crate) fn router(state: AppState) -> Router {
    Router::new()
        .route("/health/live", get(routes::health::live))
        .route("/health/ready", get(routes::health::ready))
        .route(
            "/catalogue/search",
            post(music_library::api::native::search),
        )
        .route(
            "/playback/tracks/{track_id}/stream",
            get(playback::api::native::stream),
        )
        .route(
            "/playback/tracks/{track_id}/artwork",
            get(music_library::api::native::artwork),
        )
        .nest("/rest", facades::subsonic::router())
        .nest(
            "/compat/subsonic/rest",
            facades::subsonic::router().layer(from_fn(facades::cors)),
        )
        .nest(
            "/compat/navidrome/rest",
            facades::subsonic::router().layer(from_fn(facades::cors)),
        )
        .nest(
            "/compat/navidrome",
            facades::navidrome::router().layer(from_fn(facades::cors)),
        )
        .fallback(routes::not_found)
        .layer(axum::middleware::from_fn(middleware::request_id))
        .with_state(state)
}
