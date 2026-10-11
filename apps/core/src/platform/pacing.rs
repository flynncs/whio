use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub struct CallLimiter {
    interval: Duration,
    earliest: Mutex<Option<Instant>>,
    now: Box<dyn Fn() -> Instant + Send + Sync>,
}

impl CallLimiter {
    pub fn new(interval: Duration) -> Self {
        Self::with_clock(interval, Instant::now)
    }

    pub fn with_clock(
        interval: Duration,
        now: impl Fn() -> Instant + Send + Sync + 'static,
    ) -> Self {
        Self {
            interval,
            earliest: Mutex::new(None),
            now: Box::new(now),
        }
    }

    pub async fn acquire(&self) {
        let wait = self.reserve();

        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    fn reserve(&self) -> Duration {
        self.reserve_at((self.now)())
    }

    pub(crate) fn reserve_at(&self, now: Instant) -> Duration {
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
