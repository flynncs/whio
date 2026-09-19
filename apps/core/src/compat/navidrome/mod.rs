mod auth;
mod dto;
mod errors;
mod routes;
mod service;
mod session;

use axum::{
    Router,
    routing::{get, post},
};

use crate::AppState;

pub(crate) fn router() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(auth::login))
        .route("/api/song", get(routes::song_list))
        .route("/api/song/{id}", get(routes::song_detail))
        .route("/api/album", get(routes::album_list))
        .route("/api/album/{id}", get(routes::album_detail))
        .route("/api/artist", get(routes::artist_list))
        .route("/api/artist/{id}", get(routes::artist_detail))
        .fallback(errors::not_found)
}
