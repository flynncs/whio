use axum::body::{Body, BodyDataStream};
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::OwnedSemaphorePermit;
use tokio_stream::Stream;

struct SessionStream {
    inner: BodyDataStream,
    _permit: OwnedSemaphorePermit,
}

impl Stream for SessionStream {
    type Item = Result<bytes::Bytes, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        Pin::new(&mut self.inner).poll_next(cx)
    }
}

pub(super) fn hold(body: Body, permit: OwnedSemaphorePermit) -> Body {
    Body::from_stream(SessionStream {
        inner: body.into_data_stream(),
        _permit: permit,
    })
}
