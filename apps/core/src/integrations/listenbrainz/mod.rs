use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use reqwest::{StatusCode, Url, header};
use serde::{Deserialize, Serialize};

use crate::features::identity::destinations::{
    DestinationProvider, DestinationValidator, ValidatedDestination, ValidationError,
};
use crate::features::music_library::delivery::{
    ListenSender, ScrobbleListen, SubmissionError, SubmissionOutcome,
};

pub const HOST: &str = "api.listenbrainz.org";
pub const VALIDATE_PATH: &str = "/1/validate-token";
pub const SUBMIT_PATH: &str = "/1/submit-listens";
pub const MINIMUM_INTERVAL: Duration = Duration::from_secs(1);
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(20);

const USER_AGENT: &str = concat!(
    "whio/",
    env!("CARGO_PKG_VERSION"),
    " ( https://github.com/flynncs/whio )"
);
const LISTEN_TYPE: &str = "single";

pub struct ListenBrainzAdapter {
    client: reqwest::Client,
    base_url: Url,
    limiter: Arc<CallLimiter>,
}

impl ListenBrainzAdapter {
    pub fn new() -> Result<Self, AdapterError> {
        Self::with_config(
            host_url(),
            CONNECT_TIMEOUT,
            TOTAL_TIMEOUT,
            Arc::new(CallLimiter::new(MINIMUM_INTERVAL)),
        )
    }

    pub fn with_config(
        base_url: Url,
        connect_timeout: Duration,
        total_timeout: Duration,
        limiter: Arc<CallLimiter>,
    ) -> Result<Self, AdapterError> {
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_str(USER_AGENT).map_err(|_| AdapterError::UserAgent)?,
        );

        let client = reqwest::Client::builder()
            .connect_timeout(connect_timeout)
            .timeout(total_timeout)
            .default_headers(headers)
            .build()
            .map_err(AdapterError::Client)?;

        Ok(Self {
            client,
            base_url,
            limiter,
        })
    }

    pub fn limiter(&self) -> &Arc<CallLimiter> {
        &self.limiter
    }

    async fn post<T: Serialize>(
        &self,
        path: &str,
        token: &str,
        body: &T,
    ) -> Result<reqwest::Response, reqwest::Error> {
        self.limiter.acquire().await;

        let url = self
            .base_url
            .join(path)
            .unwrap_or_else(|_| self.base_url.clone());

        self.client
            .post(url)
            .header(header::AUTHORIZATION, format!("Token {token}"))
            .json(body)
            .send()
            .await
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AdapterError {
    #[error("the destination host is not a usable URL")]
    Host,

    #[error("the destination User-Agent is not encodable")]
    UserAgent,

    #[error("the destination HTTP client could not be built")]
    Client(#[source] reqwest::Error),
}

fn host_url() -> Url {
    Url::parse(&format!("https://{HOST}/")).unwrap_or_else(|_| unreachable_url())
}

fn unreachable_url() -> Url {
    Url::parse("https://127.0.0.1/").expect("a literal URL parses")
}

#[derive(Debug)]
pub struct CallLimiter {
    interval: Duration,
    earliest: Mutex<Option<Instant>>,
}

impl CallLimiter {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            earliest: Mutex::new(None),
        }
    }

    pub async fn acquire(&self) {
        let wait = self.reserve();

        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    fn reserve(&self) -> Duration {
        let now = Instant::now();
        let mut earliest = match self.earliest.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let start = earliest
            .as_ref()
            .copied()
            .filter(|at| *at > now)
            .unwrap_or(now);

        *earliest = Some(start + self.interval);

        start.saturating_duration_since(now)
    }
}

#[derive(Serialize)]
struct SubmitRequest<'a> {
    listen_type: &'static str,
    payload: [SubmitPayload<'a>; 1],
}

#[derive(Serialize)]
struct SubmitPayload<'a> {
    listened_at: i64,
    track_metadata: TrackMetadata<'a>,
}

#[derive(Serialize)]
struct TrackMetadata<'a> {
    artist_name: &'a str,
    track_name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    release_name: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    additional_info: Option<AdditionalInfo<'a>>,
}

#[derive(Serialize)]
struct AdditionalInfo<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    recording_mbid: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    release_mbid: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    duration_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    submission_client: Option<&'a str>,
}

#[derive(Deserialize)]
struct ValidateResponse {
    valid: bool,
    #[serde(default)]
    user_name: Option<String>,
}

#[derive(Deserialize)]
struct SubmitResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    message: Option<String>,
}

fn submit_body(listen: &ScrobbleListen) -> SubmitRequest<'_> {
    let additional_info = AdditionalInfo {
        recording_mbid: listen.recording_mbid.as_deref(),
        release_mbid: listen.release_mbid.as_deref(),
        duration_ms: listen.duration_ms,
        submission_client: listen.client.as_deref(),
    };

    SubmitRequest {
        listen_type: LISTEN_TYPE,
        payload: [SubmitPayload {
            listened_at: listen.listened_at,
            track_metadata: TrackMetadata {
                artist_name: &listen.artist_name,
                track_name: &listen.track_name,
                release_name: listen.release_name.as_deref(),
                additional_info: Some(additional_info),
            },
        }],
    }
}

#[async_trait]
impl DestinationValidator for ListenBrainzAdapter {
    async fn validate(
        &self,
        provider_id: &DestinationProvider,
        token: &str,
    ) -> Result<ValidatedDestination, ValidationError> {
        if *provider_id != DestinationProvider::ListenBrainz {
            return Err(ValidationError::InvalidCredential);
        }

        let response = self
            .post(VALIDATE_PATH, token, &serde_json::json!({}))
            .await
            .map_err(|_| ValidationError::Unavailable)?;

        if response.status() == StatusCode::UNAUTHORIZED {
            return Err(ValidationError::InvalidCredential);
        }

        if !response.status().is_success() {
            return Err(ValidationError::Unavailable);
        }

        let body = response
            .json::<ValidateResponse>()
            .await
            .map_err(|_| ValidationError::Unavailable)?;

        if !body.valid {
            return Err(ValidationError::InvalidCredential);
        }

        Ok(ValidatedDestination {
            provider_id: DestinationProvider::ListenBrainz,
            username: body.user_name,
        })
    }
}

#[async_trait]
impl ListenSender for ListenBrainzAdapter {
    async fn submit(
        &self,
        credential: &str,
        listen: &ScrobbleListen,
    ) -> Result<SubmissionOutcome, SubmissionError> {
        let body = submit_body(listen);
        let response = self
            .post(SUBMIT_PATH, credential, &body)
            .await
            .map_err(|_| SubmissionError::Unavailable)?;
        let status = response.status();

        if status == StatusCode::TOO_MANY_REQUESTS {
            return Err(SubmissionError::RateLimited {
                retry_after: retry_after(&response),
            });
        }

        if status == StatusCode::UNAUTHORIZED {
            return Err(SubmissionError::CredentialRejected);
        }

        if status.is_server_error() {
            return Err(SubmissionError::Unavailable);
        }

        if !status.is_success() {
            return Err(SubmissionError::Rejected {
                message: status.to_string(),
            });
        }

        let body = response
            .json::<SubmitResponse>()
            .await
            .map_err(|_| SubmissionError::Unavailable)?;

        match body.status.as_deref() {
            Some("ok") => Ok(SubmissionOutcome::Accepted),
            Some("filtered") => Ok(SubmissionOutcome::Filtered {
                message: body
                    .message
                    .unwrap_or_else(|| "the destination filtered the listen".to_owned()),
            }),
            _ => Err(SubmissionError::Unavailable),
        }
    }
}

fn retry_after(response: &reqwest::Response) -> Option<Duration> {
    response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

#[cfg(test)]
mod tests;
