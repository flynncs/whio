use std::{sync::Arc, time::Duration};

use reqwest::{Url, header};
use serde::Serialize;
use serde_json::Value;
use tokio_stream::StreamExt;

use crate::{
    features::metadata::refresh::DocumentError,
    integrations::{
        document_http::{self, NotFoundIs},
        listenbrainz::stream,
    },
    platform::pacing::CallLimiter,
};

const API: &str = "https://api.listenbrainz.org/";
const LABS: &str = "https://labs.api.listenbrainz.org/";
const TIMEOUT: Duration = Duration::from_secs(20);
const LIMIT: usize = 50;

pub struct ListenBrainzClient {
    client: reqwest::Client,
    api: Url,
    labs: Url,
    token: String,
    user_agent: String,
    algorithm: String,
    limiter: Arc<CallLimiter>,
}

impl ListenBrainzClient {
    pub fn new(
        config: &super::documents_config::Config,
        limiter: Arc<CallLimiter>,
    ) -> Result<Self, reqwest::Error> {
        Self::with_hosts(
            Url::parse(API).expect("ListenBrainz API URL is valid"),
            Url::parse(LABS).expect("ListenBrainz Labs URL is valid"),
            config.token.clone(),
            platform_user_agent().to_owned(),
            config.algorithm.clone(),
            limiter,
        )
    }

    pub(crate) fn with_hosts(
        api: Url,
        labs: Url,
        token: String,
        user_agent: String,
        algorithm: String,
        limiter: Arc<CallLimiter>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder().timeout(TIMEOUT).build()?;
        Ok(Self {
            client,
            api,
            labs,
            token,
            user_agent,
            algorithm,
            limiter,
        })
    }

    pub fn algorithm(&self) -> &str {
        &self.algorithm
    }

    pub async fn fetch_similar_artists(&self, mbid: &str) -> Result<Value, DocumentError> {
        let url = self
            .labs
            .join("similar-artists/json")
            .map_err(|_| DocumentError::Malformed)?;
        self.json_get(
            url,
            &[("artist_mbids", mbid), ("algorithm", &self.algorithm)],
            false,
        )
        .await
    }

    pub async fn fetch_top_recordings(&self, mbid: &str) -> Result<Vec<Value>, DocumentError> {
        let url = self
            .api
            .join(&format!("1/popularity/top-recordings-for-artist/{mbid}"))
            .map_err(|_| DocumentError::Malformed)?;
        let response = self.send(self.client.get(url), true).await?;
        let mut stream_body = response.bytes_stream();
        let mut buffer = Vec::new();
        let mut cursor = 0;
        let mut opened = false;
        let mut items = Vec::new();
        while let Some(chunk) = stream_body.next().await {
            let bytes = chunk.map_err(|_| DocumentError::Unavailable { retry_after: None })?;
            if stream::append_items(
                &mut buffer,
                &mut cursor,
                &mut opened,
                &mut items,
                LIMIT,
                &bytes,
            )? {
                return Ok(items);
            }
        }
        stream::finish(&buffer, cursor, opened)?;
        Ok(items)
    }

    pub async fn fetch_artist_popularity(&self, mbid: &str) -> Result<Value, DocumentError> {
        let url = self
            .api
            .join("1/popularity/artist")
            .map_err(|_| DocumentError::Malformed)?;
        #[derive(Serialize)]
        struct Request<'a> {
            artist_mbids: [&'a str; 1],
        }
        let response = self
            .send(
                self.client.post(url).json(&Request {
                    artist_mbids: [mbid],
                }),
                true,
            )
            .await?;
        document_http::json(Ok(response), NotFoundIs::SourceNotFound).await
    }

    async fn json_get(
        &self,
        url: Url,
        params: &[(&str, &str)],
        authorized: bool,
    ) -> Result<Value, DocumentError> {
        let response = self
            .send(self.client.get(url).query(params), authorized)
            .await?;
        document_http::json(Ok(response), NotFoundIs::SourceNotFound).await
    }

    #[cfg(test)]
    fn limiter(&self) -> &Arc<CallLimiter> {
        &self.limiter
    }

    async fn send(
        &self,
        request: reqwest::RequestBuilder,
        authorized: bool,
    ) -> Result<reqwest::Response, DocumentError> {
        self.limiter.acquire().await;
        let request = request.header(header::USER_AGENT, &self.user_agent);
        let request = if authorized {
            request.header(header::AUTHORIZATION, format!("Token {}", self.token))
        } else {
            request
        };
        let sent = request.send().await.map_err(|error| error.without_url());
        document_http::prepare(sent, NotFoundIs::SourceNotFound)
    }
}

fn platform_user_agent() -> &'static str {
    crate::platform::http::USER_AGENT
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Router,
        body::Body,
        extract::State,
        http::{Request, StatusCode},
        routing::any,
    };
    use std::sync::Mutex;
    use tokio::net::TcpListener;

    type Calls = Arc<Mutex<Vec<(String, String, Option<String>)>>>;

    async fn server() -> (Url, Calls) {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .fallback(any(record))
            .with_state(Arc::clone(&calls));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (Url::parse(&format!("http://{address}/")).unwrap(), calls)
    }

    async fn record(
        State(calls): State<Calls>,
        request: Request<Body>,
    ) -> impl axum::response::IntoResponse {
        let method = request.method().to_string();
        let path = request.uri().to_string();
        let authorization = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        calls
            .lock()
            .unwrap()
            .push((method.clone(), path.clone(), authorization));
        let body = match path.split('?').next().unwrap_or(&path) {
            "/similar-artists/json" => "[]",
            "/1/popularity/top-recordings-for-artist/mbid" => {
                "[{\"recording_mbid\":\"recording\",\"recording_name\":\"Track\"}]"
            }
            "/1/popularity/artist" => "[{\"artist_mbid\":\"mbid\"}]",
            _ => "{}",
        };
        (StatusCode::OK, body)
    }

    #[tokio::test]
    async fn authorization_is_added_only_to_api_requests() {
        let (api, calls) = server().await;
        let client = ListenBrainzClient::with_hosts(
            api.clone(),
            api,
            "private-token".to_owned(),
            "ua".to_owned(),
            "algorithm".to_owned(),
            Arc::new(CallLimiter::new(Duration::ZERO)),
        )
        .unwrap();
        client.fetch_similar_artists("mbid").await.unwrap();
        client.fetch_top_recordings("mbid").await.unwrap();
        client.fetch_artist_popularity("mbid").await.unwrap();
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].2, None);
        assert_eq!(calls[1].2.as_deref(), Some("Token private-token"));
        assert_eq!(calls[2].2.as_deref(), Some("Token private-token"));
        assert_eq!(
            calls[0].1,
            "/similar-artists/json?artist_mbids=mbid&algorithm=algorithm"
        );
    }

    #[test]
    fn scrobbling_and_documents_share_one_pace() {
        let limiter = Arc::new(CallLimiter::new(super::super::MINIMUM_INTERVAL));
        let base = Url::parse("http://127.0.0.1:4321/").unwrap();
        let adapter = crate::integrations::listenbrainz::ListenBrainzAdapter::with_config(
            base.clone(),
            Duration::from_secs(1),
            Duration::from_secs(20),
            Arc::clone(&limiter),
        )
        .unwrap();
        let documents = ListenBrainzClient::with_hosts(
            base.clone(),
            base,
            "token".to_owned(),
            "ua".to_owned(),
            "algorithm".to_owned(),
            Arc::clone(&limiter),
        )
        .unwrap();
        assert!(Arc::ptr_eq(adapter.limiter(), documents.limiter()));
        let now = std::time::Instant::now();
        assert_eq!(limiter.reserve_at(now), Duration::ZERO);
        assert_eq!(limiter.reserve_at(now), super::super::MINIMUM_INTERVAL);
    }

    #[tokio::test]
    async fn a_document_fetches_only_requested_fields_and_filters_the_result() {
        let (api, calls) = server().await;
        let client = ListenBrainzClient::with_hosts(
            api.clone(),
            api,
            "token".to_owned(),
            "ua".to_owned(),
            "algorithm".to_owned(),
            Arc::new(CallLimiter::new(Duration::ZERO)),
        )
        .unwrap();
        let documents = super::super::documents_artist::ListenBrainzDocuments::with_client(client);
        let source = crate::features::music_library::domain::SourceIdentity::new(
            crate::features::music_library::domain::ProviderId::new("listenbrainz".to_owned())
                .unwrap(),
            crate::features::music_library::domain::SourceScope::Global,
            "mbid".to_owned(),
        )
        .unwrap();
        let document = documents
            .fetch_document(
                &source,
                &[crate::features::metadata::domain::FieldId::TopTracks],
            )
            .await
            .unwrap();
        assert_eq!(document.address, source);
        assert_eq!(document.candidates.len(), 1);
        assert_eq!(
            document.candidates[0].field,
            crate::features::metadata::domain::FieldId::TopTracks
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
        let empty = documents.fetch_document(&source, &[]).await.unwrap();
        assert!(empty.candidates.is_empty());
        assert!(empty.errors.is_empty());
        assert_eq!(calls.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn top_recording_response_is_incrementally_streamed() {
        let (api, _) = server().await;
        let client = ListenBrainzClient::with_hosts(
            api.clone(),
            api,
            "token".to_owned(),
            "ua".to_owned(),
            "algorithm".to_owned(),
            Arc::new(CallLimiter::new(Duration::ZERO)),
        )
        .unwrap();
        let values = client.fetch_top_recordings("mbid").await.unwrap();
        assert_eq!(
            values,
            vec![serde_json::json!({"recording_mbid":"recording","recording_name":"Track"})]
        );
    }
}
