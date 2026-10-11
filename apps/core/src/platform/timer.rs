use std::time::Duration;

use async_trait::async_trait;

#[async_trait]
pub trait Timer: Send + Sync {
    fn now(&self) -> Duration;
    async fn sleep_until(&self, deadline: Duration);
}

pub struct TokioTimer {
    start: tokio::time::Instant,
}

impl TokioTimer {
    pub fn new() -> Self {
        Self {
            start: tokio::time::Instant::now(),
        }
    }
}

impl Default for TokioTimer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Timer for TokioTimer {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    async fn sleep_until(&self, deadline: Duration) {
        tokio::time::sleep_until(self.start + deadline).await;
    }
}

#[cfg(test)]
pub(crate) mod manual {
    use std::{
        collections::BTreeMap,
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
        time::Duration,
    };

    use async_trait::async_trait;

    use super::Timer;

    pub(crate) struct ManualTimer {
        sender: tokio::sync::watch::Sender<Duration>,
        waits: tokio::sync::watch::Sender<BTreeMap<u64, Duration>>,
        next_wait: AtomicU64,
    }

    struct WaitRegistration<'a> {
        timer: &'a ManualTimer,
        id: u64,
    }

    impl Drop for WaitRegistration<'_> {
        fn drop(&mut self) {
            self.timer.waits.send_modify(|waits| {
                waits.remove(&self.id);
            });
        }
    }

    impl ManualTimer {
        pub(crate) fn new() -> Arc<Self> {
            let (sender, _) = tokio::sync::watch::channel(Duration::ZERO);

            let (waits, _) = tokio::sync::watch::channel(BTreeMap::new());

            Arc::new(Self {
                sender,
                waits,
                next_wait: AtomicU64::new(0),
            })
        }

        pub(crate) fn advance(&self, by: Duration) {
            self.sender.send_modify(|now| *now += by);
        }

        pub(crate) async fn await_waiter(&self) {
            self.await_wait(|waits| !waits.is_empty()).await;
        }

        pub(crate) async fn await_deadline(&self, deadline: Duration) {
            self.await_wait(|waits| waits.values().any(|value| *value == deadline))
                .await;
        }

        pub(crate) fn waiting(&self) -> usize {
            self.waits.borrow().len()
        }

        async fn await_wait(&self, ready: impl Fn(&BTreeMap<u64, Duration>) -> bool) {
            let mut waits = self.waits.subscribe();
            loop {
                if ready(&waits.borrow_and_update()) {
                    return;
                }

                waits.changed().await.unwrap();
            }
        }
    }

    #[async_trait]
    impl Timer for ManualTimer {
        fn now(&self) -> Duration {
            *self.sender.borrow()
        }

        async fn sleep_until(&self, deadline: Duration) {
            let mut receiver = self.sender.subscribe();

            if *receiver.borrow_and_update() >= deadline {
                return;
            }

            let registration = WaitRegistration {
                timer: self,
                id: self.next_wait.fetch_add(1, Ordering::Relaxed),
            };
            self.waits.send_modify(|waits| {
                waits.insert(registration.id, deadline);
            });

            loop {
                if *receiver.borrow_and_update() >= deadline {
                    break;
                }
                if receiver.changed().await.is_err() {
                    break;
                }
            }

            drop(registration);
        }
    }
}
