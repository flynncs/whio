use std::{
    collections::BTreeSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
pub const CAPABILITY_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CapabilityError {
    #[error("the resolver is not configured")]
    Disabled,
    #[error("the resolver could not be reached")]
    Unavailable,
    #[error("the resolver answered malformed capabilities")]
    InvalidData,
    #[error("source `{provider_id}` is not advertised")]
    Unadvertised { provider_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Snapshot {
    providers: BTreeSet<String>,
}

impl Snapshot {
    pub fn new(providers: impl IntoIterator<Item = String>) -> Self {
        Self {
            providers: providers.into_iter().collect(),
        }
    }

    pub fn advertised(&self, provider_id: &str) -> bool {
        self.providers.contains(provider_id)
    }

    pub fn enabled_sources(&self, order: &[String]) -> Vec<String> {
        order
            .iter()
            .filter(|provider_id| self.advertised(provider_id))
            .cloned()
            .collect()
    }

    pub fn providers(&self) -> impl Iterator<Item = &str> {
        self.providers.iter().map(String::as_str)
    }
}

#[async_trait]
pub trait CapabilitySource: Send + Sync {
    async fn fetch(&self) -> Result<Snapshot, CapabilityError>;
}

pub struct CapabilitySnapshot {
    source: Arc<dyn CapabilitySource>,
    ttl: Duration,
    held: Mutex<Option<(DateTime<Utc>, Snapshot)>>,
}

impl CapabilitySnapshot {
    pub fn new(source: Arc<dyn CapabilitySource>, ttl: Duration) -> Self {
        Self {
            source,
            ttl,
            held: Mutex::new(None),
        }
    }

    pub fn held(&self, now: DateTime<Utc>) -> Option<Snapshot> {
        let guard = self.held.lock().ok()?;
        let (fetched_at, snapshot) = guard.as_ref()?;
        let age = now.signed_duration_since(*fetched_at).num_seconds();

        (age >= 0 && age < self.ttl.as_secs() as i64).then(|| snapshot.clone())
    }

    pub async fn current(&self, now: DateTime<Utc>) -> Result<Snapshot, CapabilityError> {
        if let Some(snapshot) = self.held(now) {
            return Ok(snapshot);
        }

        let snapshot = self.source.fetch().await?;

        if let Ok(mut guard) = self.held.lock() {
            *guard = Some((now, snapshot.clone()));
        }

        Ok(snapshot)
    }

    pub async fn advertised(&self, provider_id: &str, now: DateTime<Utc>) -> bool {
        self.current(now)
            .await
            .is_ok_and(|snapshot| snapshot.advertised(provider_id))
    }

    pub async fn enabled_sources(&self, order: &[String], now: DateTime<Utc>) -> Vec<String> {
        self.current(now)
            .await
            .map(|snapshot| snapshot.enabled_sources(order))
            .unwrap_or_default()
    }

    pub async fn accept(
        &self,
        provider_id: &str,
        now: DateTime<Utc>,
    ) -> Result<(), CapabilityError> {
        let snapshot = self.current(now).await?;

        if snapshot.advertised(provider_id) {
            Ok(())
        } else {
            Err(CapabilityError::Unadvertised {
                provider_id: provider_id.to_owned(),
            })
        }
    }
}
