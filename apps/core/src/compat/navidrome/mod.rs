mod auth;
mod dto;
mod errors;
mod routes;
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
        .fallback(errors::not_found)
}
