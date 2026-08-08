use axum::Router;

pub mod catalogue;
pub mod resolvers;
pub use resolvers as resolver;

pub fn router() -> Router {
    http::router()
}

mod http;
