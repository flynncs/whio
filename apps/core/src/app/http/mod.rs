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
            "/api/v1/library/tracks",
            get(music_library::api::native::library_tracks),
        )
        .route(
            "/api/v1/library/albums",
            get(music_library::api::native::library_albums),
        )
        .route(
            "/api/v1/library/artists",
            get(music_library::api::native::library_artists),
        )
        .route(
            "/api/v1/playlists",
            get(music_library::api::playlists::list).post(music_library::api::playlists::create),
        )
        .route(
            "/api/v1/playlists/{id}",
            get(music_library::api::playlists::detail)
                .patch(music_library::api::playlists::update)
                .delete(music_library::api::playlists::remove),
        )
        .route(
            "/api/v1/playlists/{id}/entries/{entry_id}/move",
            post(music_library::api::playlists::move_entry),
        )
        .route(
            "/api/v1/queue",
            get(music_library::api::queue::read)
                .put(music_library::api::queue::save)
                .delete(music_library::api::queue::clear),
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
            facades::navidrome::router::<AppState>().layer(from_fn(facades::cors)),
        )
        .fallback(routes::not_found)
        .layer(axum::middleware::from_fn(middleware::request_id))
        .with_state(state)
}
