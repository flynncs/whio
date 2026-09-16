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
        .fallback(errors::not_found)
}
