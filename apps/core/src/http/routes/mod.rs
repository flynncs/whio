use axum::Extension;

use super::errors::ApiError;
use crate::request::RequestContext;

pub(crate) mod catalogue;
pub(crate) mod health;

pub(crate) async fn not_found(Extension(context): Extension<RequestContext>) -> ApiError {
    ApiError::NotFound(context)
}
