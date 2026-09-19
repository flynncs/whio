use axum::response::Response;

use crate::playback::DeliveryResult;

pub fn into_response(delivery: DeliveryResult) -> Response {
    let mut response = Response::new(delivery.body);
    *response.status_mut() = delivery.status;
    *response.headers_mut() = delivery.headers;
    response
}
