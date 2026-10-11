pub mod api;
pub mod config;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Url;
use uuid::Uuid;

use crate::features::acquisition::ports::{
    BackendHealth, CandidateLimit, CandidateSearch, ReleaseCandidate, RemoteTransfer, SearchText,
    Slot, TransferError, TransferHandle, TransferProgress, TransferRequest,
};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_secs(1);
const SEARCH_GRACE: Duration = Duration::from_secs(5);

pub struct Slskd {
    pub(super) client: reqwest::Client,
    pub(super) base: Url,
    pub(super) api_key: String,
    pub(super) search_timeout: Duration,
    pub(super) poll_interval: Duration,
    pub(super) search_grace: Duration,
    pub(super) timer: std::sync::Arc<dyn crate::platform::timer::Timer>,
}

impl Slskd {
    pub fn new(config: &config::Config) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(crate::platform::http::USER_AGENT)
            .build()?;

        Ok(Self {
            client,
            base: config.url.clone(),
            api_key: config.api_key.clone(),
            search_timeout: config.search_timeout,
            poll_interval: POLL_INTERVAL,
            search_grace: SEARCH_GRACE,
            timer: std::sync::Arc::new(crate::platform::timer::TokioTimer::new()),
        })
    }

    #[cfg(test)]
    pub(crate) fn with_config(
        base: Url,
        api_key: String,
        search_timeout: Duration,
        poll_interval: Duration,
        search_grace: Duration,
        timer: std::sync::Arc<dyn crate::platform::timer::Timer>,
    ) -> Result<Self, reqwest::Error> {
        let client = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .user_agent(crate::platform::http::USER_AGENT)
            .build()?;

        Ok(Self {
            client,
            base,
            api_key,
            search_timeout,
            poll_interval,
            search_grace,
            timer,
        })
    }
}

#[async_trait]
impl CandidateSearch for Slskd {
    #[tracing::instrument(skip_all, fields(backend = "slskd", limit = limit.get()))]
    async fn search(
        &self,
        query: &SearchText,
        limit: CandidateLimit,
    ) -> Result<Vec<ReleaseCandidate>, TransferError> {
        let id = Uuid::now_v7();
        self.create_search(id, query, limit).await?;
        let deadline = self.timer.now() + self.search_timeout + self.search_grace;

        let polled = loop {
            match self.search_complete(id).await {
                Ok(true) => break Ok(()),
                Ok(false) => {}
                Err(error) => break Err(error),
            }

            let now = self.timer.now();
            if now >= deadline {
                break Ok(());
            }

            let next = now + self.poll_interval;
            self.timer
                .sleep_until(if next < deadline { next } else { deadline })
                .await;
        };

        let candidates = match polled {
            Ok(()) => self.search_candidates(id).await,
            Err(error) => Err(error),
        };
        self.delete_search(id).await;

        candidates.map(|candidates| {
            let mut candidates = candidates;
            candidates.truncate(usize::from(limit.get()));

            candidates
        })
    }
}

#[async_trait]
impl RemoteTransfer for Slskd {
    #[tracing::instrument(skip_all, fields(backend = "slskd", count = requests.len()))]
    async fn start(
        &self,
        requests: &[TransferRequest],
    ) -> Result<Vec<Slot<TransferHandle>>, TransferError> {
        let mut slots = Vec::with_capacity(requests.len());

        for request in requests {
            slots.push(self.enqueue(request).await?);
        }

        Ok(slots)
    }

    #[tracing::instrument(skip_all, fields(backend = "slskd", count = handles.len()))]
    async fn status(
        &self,
        handles: &[TransferHandle],
    ) -> Result<Vec<Slot<TransferProgress>>, TransferError> {
        let mut slots = Vec::with_capacity(handles.len());

        for handle in handles {
            slots.push(self.batch_progress(handle).await?);
        }

        Ok(slots)
    }

    #[tracing::instrument(skip_all, fields(backend = "slskd", remove = remove))]
    async fn cancel(&self, handle: &TransferHandle, remove: bool) -> Result<(), TransferError> {
        self.cancel_transfer(handle, remove).await
    }

    async fn health(&self) -> BackendHealth {
        match self.logged_in().await {
            Ok(true) => BackendHealth::Ready,
            Ok(false) => BackendHealth::NotLoggedIn,
            Err(TransferError::CredentialsRejected) => BackendHealth::CredentialsRejected,
            Err(_) => BackendHealth::Unavailable,
        }
    }
}
