use std::{str::FromStr, time::Duration};

use chrono::{DateTime, Utc};

use crate::features::metadata::domain::{
    AbsenceReason, DESCRIPTIVE_TTL, DISCOVERY_TTL, FieldId, Freshness, NEGATIVE_TTL,
    RELATIONSHIP_TTL, StoredAttempt, StoredFact,
};

pub const FAILURE_FLOOR: Duration = Duration::from_secs(60 * 60);
pub const JITTER_PERCENT: u32 = 20;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown metadata field `{name}`")]
pub struct UnknownField {
    name: String,
}

impl UnknownField {
    pub fn name(&self) -> &str {
        &self.name
    }
}

pub fn ttl_for(field: &str) -> Result<Duration, UnknownField> {
    let field = FieldId::from_str(field).map_err(|_| UnknownField {
        name: field.to_owned(),
    })?;

    Ok(match field {
        FieldId::IdentityName | FieldId::IdentityMbid => RELATIONSHIP_TTL,
        FieldId::Similar
        | FieldId::TopTracks
        | FieldId::MetricListeners
        | FieldId::MetricPlays
        | FieldId::MetricListen
        | FieldId::MetricListener => DISCOVERY_TTL,
        _ => DESCRIPTIVE_TTL,
    })
}

pub trait Jitter: Send + Sync {
    fn jitter(&self) -> f64;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct NoJitter;

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemJitter;

impl SystemJitter {
    pub fn new() -> Self {
        Self
    }
}

impl Jitter for SystemJitter {
    fn jitter(&self) -> f64 {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.subsec_nanos())
            .unwrap_or(0);

        (f64::from(nanos) / 1_000_000_000.0) * 2.0 - 1.0
    }
}

impl Jitter for NoJitter {
    fn jitter(&self) -> f64 {
        0.0
    }
}

#[derive(Debug, Clone, Copy)]
pub struct FixedJitter(f64);

impl FixedJitter {
    pub fn new(value: f64) -> Self {
        Self(value)
    }
}

impl Jitter for FixedJitter {
    fn jitter(&self) -> f64 {
        self.0
    }
}

pub fn jittered(ttl: Duration, jitter: &dyn Jitter) -> Duration {
    let bound = ttl.as_secs_f64() * f64::from(JITTER_PERCENT) / 100.0;
    let offset = bound * jitter.jitter().clamp(-1.0, 1.0);
    let millis = ((ttl.as_secs_f64() + offset).max(0.0) * 1000.0).round();

    Duration::from_millis(millis as u64)
}

pub fn freshness(
    latest_fact_at: Option<DateTime<Utc>>,
    latest_attempt: Option<&StoredAttempt>,
    ttl: Duration,
    now: DateTime<Utc>,
) -> Freshness {
    let last_success = success_at(latest_fact_at, latest_attempt);

    if let Some(message) = failure_message(last_success, latest_attempt) {
        return Freshness::Failed(message);
    }

    let Some(success) = last_success else {
        return Freshness::Unrequested;
    };

    if now < success {
        return Freshness::Fresh;
    }

    let effective = if latest_fact_at.is_none() {
        NEGATIVE_TTL
    } else {
        ttl
    };

    if now.signed_duration_since(success).num_seconds() < effective.as_secs() as i64 {
        Freshness::Fresh
    } else {
        Freshness::Stale
    }
}

pub fn absence(
    facts: &[StoredFact],
    attempts: &[StoredAttempt],
    ttl: Duration,
    now: DateTime<Utc>,
) -> Option<AbsenceReason> {
    let latest_attempt = attempts.iter().max_by_key(|attempt| attempt.attempted_at);
    let last_success = success_at(
        facts.iter().map(|fact| fact.fetched_at).max(),
        latest_attempt,
    );

    if failure_message(last_success, latest_attempt).is_some() {
        return None;
    }

    let Some(success) = last_success else {
        return Some(AbsenceReason::Unrequested);
    };

    let effective = if facts.is_empty() { NEGATIVE_TTL } else { ttl };

    if now.signed_duration_since(success).num_seconds() >= effective.as_secs() as i64 {
        Some(AbsenceReason::StaleAbsent)
    } else {
        Some(AbsenceReason::ProviderReportedNone)
    }
}

pub fn is_due(
    facts: &[StoredFact],
    attempts: &[StoredAttempt],
    ttl: Duration,
    jitter: &dyn Jitter,
    now: DateTime<Utc>,
) -> bool {
    let latest_attempt = attempts.iter().max_by_key(|attempt| attempt.attempted_at);
    let last_success = success_at(
        facts.iter().map(|fact| fact.fetched_at).max(),
        latest_attempt,
    );

    if failure_message(last_success, latest_attempt).is_some()
        && let Some(attempted_at) = latest_attempt.map(|attempt| attempt.attempted_at)
    {
        let floor = attempted_at + offset(FAILURE_FLOOR);
        let window = attempted_at + offset(jittered(FAILURE_FLOOR, jitter));

        return now >= floor.max(window);
    }

    let effective = if facts.is_empty() {
        jittered(NEGATIVE_TTL, jitter)
    } else {
        jittered(ttl, jitter)
    };
    let Some(success) = last_success else {
        return true;
    };

    now.signed_duration_since(success).num_seconds() >= effective.as_secs() as i64
}

fn offset(duration: Duration) -> chrono::Duration {
    chrono::Duration::from_std(duration).unwrap_or_default()
}

fn success_at(
    latest_fact_at: Option<DateTime<Utc>>,
    latest_attempt: Option<&StoredAttempt>,
) -> Option<DateTime<Utc>> {
    latest_fact_at
        .into_iter()
        .chain(
            latest_attempt
                .iter()
                .filter_map(|attempt| attempt.fetched_at),
        )
        .max()
}

fn failure_message(
    last_success: Option<DateTime<Utc>>,
    latest_attempt: Option<&StoredAttempt>,
) -> Option<String> {
    latest_attempt
        .filter(|attempt| attempt.error.is_some())
        .filter(|attempt| last_success.is_none_or(|success| attempt.attempted_at > success))
        .and_then(|attempt| attempt.error.as_ref())
        .map(|error| error.message.clone())
}
