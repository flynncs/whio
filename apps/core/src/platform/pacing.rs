use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

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
        self.reserve_at(Instant::now())
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
