use std::time::{Duration, Instant};

pub(crate) struct TransferStats {
    bytes: u64,
    elapsed: Duration,
}

impl TransferStats {
    pub(crate) fn since(bytes: u64, started_at: Instant) -> Self {
        Self {
            bytes,
            elapsed: started_at.elapsed(),
        }
    }

    pub(crate) fn elapsed_ms(&self) -> u128 {
        self.elapsed.as_millis()
    }

    pub(crate) fn average_mbps(&self) -> f64 {
        let seconds = self.elapsed.as_secs_f64();
        if seconds == 0.0 {
            return 0.0;
        }

        self.bytes as f64 * 8.0 / seconds / 1_000_000.0
    }
}
