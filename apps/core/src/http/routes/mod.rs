use axum::Extension;

use super::errors::ApiError;
use super::middleware::RequestId;

pub(crate) mod health;

pub(crate) async fn not_found(Extension(request_id): Extension<RequestId>) -> ApiError {
    ApiError::NotFound(request_id)
}
