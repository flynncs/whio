use std::{sync::Arc, time::Duration};

use reqwest::{Url, header};
use serde_json::Value;

use crate::{
    features::metadata::refresh::DocumentError,
    integrations::document_http::{self, NotFoundIs},
    platform::{self, pacing::CallLimiter},
};

const BASE_URL: &str = "https://www.theaudiodb.com/";
const PACE: Duration = Duration::from_secs(3);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);

pub struct AudioDbClient {
    client: reqwest::Client,
    base_url: Url,
    key: String,
    limiter: Arc<CallLimiter>,
}

impl AudioDbClient {
    pub fn new(key: String) -> Result<Self, reqwest::Error> {
        let base_url = Url::parse(BASE_URL).expect("the AudioDB base URL is valid");
        Self::with_config(base_url, key, Arc::new(CallLimiter::new(PACE)))
    }

    pub(crate) fn with_config(
        base_url: Url,
        key: String,
        limiter: Arc<CallLimiter>,
    ) -> Result<Self, reqwest::Error> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(platform::http::USER_AGENT),
        );
        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(TOTAL_TIMEOUT)
            .default_headers(headers)
            .build()?;
        Ok(Self {
            client,
            base_url,
            key,
            limiter,
        })
    }

    pub async fn fetch_artist_by_mbid(&self, mbid: &str) -> Result<Value, DocumentError> {
        self.limiter.acquire().await;
        let mut url = self.base_url.clone();
        url.path_segments_mut()
            .map_err(|_| DocumentError::Malformed)?
            .pop_if_empty()
            .push("api")
            .push("v1")
            .push("json")
            .push(&self.key)
            .push("artist-mb.php");
        let sent = self
            .client
            .get(url)
            .query(&[("i", mbid)])
            .send()
            .await
            .map_err(|error| error.without_url());
        document_http::json(sent, NotFoundIs::ResolutionFailed).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, extract::State, http::StatusCode, routing::get};
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    async fn stub(status: StatusCode) -> (Url, Arc<Mutex<Vec<String>>>) {
        let paths = Arc::new(Mutex::new(Vec::new()));
        let app =
            Router::new()
                .route(
                    "/api/v1/json/{key}/artist-mb.php",
                    get(
                        move |State(paths): State<Arc<Mutex<Vec<String>>>>,
                              uri: axum::http::Uri| async move {
                            paths.lock().unwrap().push(uri.to_string());
                            (status, axum::Json(serde_json::json!({"artists": []})))
                        },
                    ),
                )
                .with_state(Arc::clone(&paths));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (Url::parse(&format!("http://{address}/")).unwrap(), paths)
    }

    #[tokio::test]
    async fn the_artist_route_carries_key_as_one_path_segment_and_the_mbid_as_query() {
        let (base, paths) = stub(StatusCode::OK).await;
        let client = AudioDbClient::with_config(
            base,
            "key/with spaces".to_owned(),
            Arc::new(CallLimiter::new(Duration::ZERO)),
        )
        .unwrap();
        assert_eq!(
            client.fetch_artist_by_mbid("artist-mbid").await.unwrap(),
            serde_json::json!({"artists": []})
        );
        assert_eq!(
            paths.lock().unwrap().as_slice(),
            ["/api/v1/json/key%2Fwith%20spaces/artist-mb.php?i=artist-mbid"]
        );
    }

    #[tokio::test]
    async fn the_audio_db_key_is_not_returned_in_http_failures() {
        let (base, _) = stub(StatusCode::BAD_REQUEST).await;
        let secret = "private-key-marker";
        let client = AudioDbClient::with_config(
            base,
            secret.to_owned(),
            Arc::new(CallLimiter::new(Duration::ZERO)),
        )
        .unwrap();
        let error = client
            .fetch_artist_by_mbid("artist-mbid")
            .await
            .unwrap_err();
        assert_eq!(error, DocumentError::ResolutionFailed);
        assert!(!format!("{error:?}").contains(secret));
    }
}
