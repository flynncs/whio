use std::pin::Pin;
use std::task::{Context, Poll};

use bytes::Bytes;
use tokio_stream::Stream;

use super::MediaFetchError;
use super::ports::{MediaBody, MediaByteStream};

const PAD_CHUNK_BYTES: u64 = 64 * 1024;

pub fn fit_to_length(body: MediaBody, length: u64) -> MediaBody {
    MediaBody {
        content_length: Some(length),
        chunks: Box::pin(FitStream {
            inner: body.chunks,
            remaining: length,
            state: FitState::Emitting,
        }),
    }
}

enum FitState {
    Emitting,
    Padding,
    Draining,
}

struct FitStream {
    inner: MediaByteStream,
    remaining: u64,
    state: FitState,
}

impl Stream for FitStream {
    type Item = Result<Bytes, MediaFetchError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match this.state {
                FitState::Emitting if this.remaining == 0 => this.state = FitState::Draining,
                FitState::Padding if this.remaining == 0 => return Poll::Ready(None),
                FitState::Draining => match this.inner.as_mut().poll_next(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(error))),
                    Poll::Ready(Some(Ok(_excess))) => continue,
                },
                FitState::Padding => return Poll::Ready(Some(Ok(pad(&mut this.remaining)))),
                FitState::Emitting => match this.inner.as_mut().poll_next(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Some(Err(error))) => return Poll::Ready(Some(Err(error))),
                    Poll::Ready(None) => this.state = FitState::Padding,
                    Poll::Ready(Some(Ok(bytes))) => {
                        let available = bytes.len() as u64;
                        if available <= this.remaining {
                            this.remaining -= available;
                            return Poll::Ready(Some(Ok(bytes)));
                        }
                        let kept = this.remaining as usize;
                        this.remaining = 0;
                        this.state = FitState::Draining;
                        return Poll::Ready(Some(Ok(bytes.slice(0..kept))));
                    }
                },
            }
        }
    }
}

fn pad(remaining: &mut u64) -> Bytes {
    let size = (*remaining).min(PAD_CHUNK_BYTES) as usize;
    *remaining -= size as u64;
    Bytes::from(vec![0u8; size])
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::sync::Notify;
    use tokio_stream::StreamExt;

    use super::*;

    fn body(chunks: Vec<Result<Bytes, MediaFetchError>>) -> MediaBody {
        MediaBody {
            content_length: None,
            chunks: Box::pin(tokio_stream::iter(chunks)),
        }
    }

    async fn collect(body: MediaBody) -> Result<Vec<u8>, MediaFetchError> {
        let mut stream = body.chunks;
        let mut collected = Vec::new();
        while let Some(chunk) = stream.next().await {
            collected.extend_from_slice(&chunk?);
        }
        Ok(collected)
    }

    #[tokio::test]
    async fn a_short_output_is_padded_to_the_length() {
        let fitted = fit_to_length(body(vec![Ok(Bytes::from_static(b"0123"))]), 10);
        assert_eq!(fitted.content_length, Some(10));
        assert_eq!(collect(fitted).await.unwrap(), b"0123\0\0\0\0\0\0");
    }

    #[tokio::test]
    async fn a_long_output_is_trimmed_to_the_length() {
        let fitted = fit_to_length(body(vec![Ok(Bytes::from_static(b"0123456789"))]), 4);
        assert_eq!(collect(fitted).await.unwrap(), b"0123");

        let fitted = fit_to_length(
            body(vec![
                Ok(Bytes::from_static(b"012")),
                Ok(Bytes::from_static(b"345")),
            ]),
            5,
        );
        assert_eq!(collect(fitted).await.unwrap(), b"01234");
    }

    #[tokio::test]
    async fn an_exact_output_is_unchanged() {
        let fitted = fit_to_length(body(vec![Ok(Bytes::from_static(b"0123456789"))]), 10);
        assert_eq!(collect(fitted).await.unwrap(), b"0123456789");
    }

    #[tokio::test]
    async fn a_body_error_before_the_length_is_forwarded() {
        let fitted = fit_to_length(
            body(vec![
                Ok(Bytes::from_static(b"0123")),
                Err(MediaFetchError::Conversion),
            ]),
            10,
        );
        assert_eq!(
            collect(fitted).await.unwrap_err().to_string(),
            "media conversion failed"
        );
    }

    #[tokio::test]
    async fn an_exact_prefix_followed_by_an_error_is_forwarded() {
        let fitted = fit_to_length(
            body(vec![
                Ok(Bytes::from_static(b"0123456789")),
                Err(MediaFetchError::Conversion),
            ]),
            10,
        );
        assert_eq!(
            collect(fitted).await.unwrap_err().to_string(),
            "media conversion failed"
        );
    }

    #[tokio::test]
    async fn an_oversized_prefix_followed_by_an_error_is_forwarded() {
        let fitted = fit_to_length(
            body(vec![
                Ok(Bytes::from_static(b"0123456789abc")),
                Err(MediaFetchError::Conversion),
            ]),
            10,
        );
        assert_eq!(
            collect(fitted).await.unwrap_err().to_string(),
            "media conversion failed"
        );
    }

    #[tokio::test]
    async fn a_terminal_error_after_the_length_is_reported_before_eof() {
        let release = std::sync::Arc::new(Notify::new());
        let (sender, receiver) = tokio::sync::mpsc::channel(4);
        let held = std::sync::Arc::clone(&release);
        tokio::spawn(async move {
            sender
                .send(Ok(Bytes::from_static(b"0123456789")))
                .await
                .unwrap();
            held.notified().await;
            sender.send(Err(MediaFetchError::Conversion)).await.unwrap();
        });
        let fitted = fit_to_length(
            MediaBody {
                content_length: None,
                chunks: Box::pin(tokio_stream::wrappers::ReceiverStream::new(receiver)),
            },
            10,
        );
        let mut stream = fitted.chunks;
        assert_eq!(
            stream.next().await.unwrap().unwrap(),
            Bytes::from_static(b"0123456789")
        );
        release.notify_one();
        let error = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert_eq!(error.to_string(), "media conversion failed");
    }
}
