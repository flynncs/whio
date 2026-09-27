mod auth;
mod dto;
mod errors;
mod routes;
mod service;
mod session;
pub(crate) mod state;

use axum::{
    Router,
    extract::FromRef,
    routing::{get, post, put},
};

pub(crate) fn router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    state::NavidromeState: FromRef<S>,
    state::NavidromeQueueState: FromRef<S>,
{
    Router::new()
        .route("/auth/login", post(auth::login))
        .route("/api/song", get(routes::song_list))
        .route("/api/song/{id}", get(routes::song_detail))
        .route("/api/album", get(routes::album_list))
        .route("/api/album/{id}", get(routes::album_detail))
        .route("/api/artist", get(routes::artist_list))
        .route("/api/artist/{id}", get(routes::artist_detail))
        .route("/api/genre", get(routes::genre_list))
        .route(
            "/api/playlist",
            get(routes::playlist_list).post(routes::playlist_create),
        )
        .route(
            "/api/playlist/{id}",
            get(routes::playlist_detail)
                .put(routes::playlist_update)
                .delete(routes::playlist_delete),
        )
        .route(
            "/api/playlist/{id}/tracks",
            get(routes::playlist_tracks)
                .post(routes::playlist_add_tracks)
                .delete(routes::playlist_remove_tracks),
        )
        .route(
            "/api/playlist/{id}/tracks/{item_id}",
            put(routes::playlist_move_track),
        )
        .merge(queue_router::<S>())
        .fallback(errors::not_found)
}

pub(crate) fn queue_router<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
    state::NavidromeQueueState: FromRef<S>,
{
    Router::new()
        .route("/api/queue", get(routes::queue_read))
        .route("/api/queue", post(routes::queue_save))
}

#[cfg(test)]
#[path = "queue_tests.rs"]
mod queue_tests;
