mod errors;
mod middleware;
mod routes;

use axum::{Router, routing::get};

pub(crate) fn router() -> Router {
    Router::new()
        .route("/health/live", get(routes::health::live))
        .route("/health/ready", get(routes::health::ready))
        .fallback(routes::not_found)
        .layer(axum::middleware::from_fn(middleware::request_id))
}
