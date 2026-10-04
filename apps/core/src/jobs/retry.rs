use std::time::Duration;

use awa::{JobContext, JobError, JobResult, Worker};

#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryPolicy {
    base_delay: Duration,
    max_delay: Duration,
}

impl RetryPolicy {
    pub(crate) fn new(base_delay: Duration, max_delay: Duration) -> Self {
        Self {
            base_delay,
            max_delay,
        }
    }

    pub(crate) fn delay(self, attempt: i16) -> Duration {
        let exponent = i32::from(attempt.max(0));
        let base = self.base_delay.as_secs_f64() * 2_f64.powi(exponent);
        let jitter = (uuid::Uuid::now_v7().as_u128() as u64 % 1_000_001) as f64 / 1_000_000.0;
        Duration::from_secs_f64((base * (1.0 + jitter * 0.25)).min(self.max_delay.as_secs_f64()))
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::new(Duration::from_secs(1), Duration::from_secs(24 * 60 * 60))
    }
}

pub(crate) struct RetryableWorker<W> {
    worker: W,
    policy: RetryPolicy,
}

impl<W> RetryableWorker<W> {
    pub(crate) fn new(worker: W, policy: RetryPolicy) -> Self {
        Self { worker, policy }
    }
}

#[async_trait::async_trait]
impl<W: Worker + Send + Sync> Worker for RetryableWorker<W> {
    fn kind(&self) -> &'static str {
        self.worker.kind()
    }

    async fn perform(&self, ctx: &JobContext) -> Result<JobResult, JobError> {
        match self.worker.perform(ctx).await {
            Err(JobError::Retryable(_)) if ctx.job.attempt < ctx.job.max_attempts => {
                Ok(JobResult::RetryAfter(self.policy.delay(ctx.job.attempt)))
            }
            result => result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_matches_awa_backoff_envelope() {
        let policy = RetryPolicy::default();
        for attempt in 1..=15 {
            let delay = policy.delay(attempt);
            let base = Duration::from_secs(2_u64.pow(attempt as u32));
            assert!(
                delay >= base,
                "attempt {attempt} delay {delay:?} is below {base:?}"
            );
            assert!(
                delay <= base.mul_f64(1.25),
                "attempt {attempt} delay {delay:?} exceeds the Awa jitter envelope"
            );
        }
        assert!(policy.delay(16) <= Duration::from_secs(24 * 60 * 60));
    }
}
