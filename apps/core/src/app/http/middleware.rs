use std::{net::SocketAddr, sync::Arc};

use axum::{
    extract::{ConnectInfo, Extension, Request, State},
    http::HeaderValue,
    middleware::Next,
    response::Response as AxumResponse,
};
use tracing::Instrument;

use crate::platform::{
    client_ip::{self, ClientIpPolicy},
    request::{RequestContext, RequestId},
};

pub(crate) async fn request_id(
    State(policy): State<Arc<ClientIpPolicy>>,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    mut request: Request,
    next: Next,
) -> AxumResponse {
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| RequestId::new(value.to_owned()).ok())
        .unwrap_or_else(RequestId::generate);

    let context = match peer {
        Some(Extension(ConnectInfo(peer))) => RequestContext::new(request_id.clone())
            .with_client_ip(client_ip::client_ip(
                peer,
                request.headers().get("x-forwarded-for"),
                &policy,
            )),
        None => RequestContext::new(request_id.clone()),
    };
    request.extensions_mut().insert(context);

    let span = tracing::info_span!("http_request", request_id = %request_id.as_str(),);

    let mut response = next.run(request).instrument(span).await;

    let header_val =
        HeaderValue::from_str(request_id.as_str()).expect("request ID was already validated");

    response.headers_mut().insert("x-request-id", header_val);

    response
}
