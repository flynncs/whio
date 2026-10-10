use std::{collections::HashMap, net::IpAddr, num::NonZeroU32, sync::Mutex, time::Duration};

use chrono::{DateTime, Duration as ChronoDuration, Utc};

use crate::platform::clock::Clock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LoginThrottleConfig {
    pub burst: NonZeroU32,
    pub replenish: Duration,
}

impl Default for LoginThrottleConfig {
    fn default() -> Self {
        Self {
            burst: NonZeroU32::new(10).expect("positive login burst"),
            replenish: Duration::from_secs(60),
        }
    }
}

#[derive(Hash, PartialEq, Eq)]
enum ThrottleKey {
    Account(String),
    Ip(IpAddr),
}

struct Spent {
    failures: u32,
    since: DateTime<Utc>,
}

pub struct LoginThrottle {
    config: LoginThrottleConfig,
    clock: Clock,
    entries: Mutex<HashMap<ThrottleKey, Spent>>,
}

impl LoginThrottle {
    pub fn new(config: LoginThrottleConfig, clock: Clock) -> Self {
        Self {
            config,
            clock,
            entries: Mutex::new(HashMap::new()),
        }
    }

    pub fn allowed(&self, account: Option<&str>, ip: Option<IpAddr>) -> bool {
        let keys = throttle_keys(account, ip);
        let now = self.clock.now();
        let entries = self.entries.lock().expect("login throttle lock");
        keys.iter().all(|key| {
            entries
                .get(key)
                .map(|spent| self.current(spent, now).failures < self.config.burst.get())
                .unwrap_or(true)
        })
    }

    pub fn record_failure(&self, account: Option<&str>, ip: Option<IpAddr>) {
        let now = self.clock.now();
        let keys = throttle_keys(account, ip);
        let mut entries = self.entries.lock().expect("login throttle lock");
        for key in keys {
            let spent = entries
                .get(&key)
                .map(|spent| self.current(spent, now))
                .unwrap_or(Spent {
                    failures: 0,
                    since: now,
                });
            entries.insert(
                key,
                Spent {
                    failures: spent.failures.saturating_add(1),
                    since: spent.since,
                },
            );
        }
        if entries.len() > 10_000 {
            entries.retain(|_, spent| self.current(spent, now).failures > 0);
        }
    }

    fn current(&self, spent: &Spent, now: DateTime<Utc>) -> Spent {
        let elapsed = now
            .signed_duration_since(spent.since)
            .to_std()
            .unwrap_or_default();
        let intervals = elapsed.as_nanos() / self.config.replenish.as_nanos();
        let replenished = intervals.min(u32::MAX as u128) as u32;
        let failures = spent.failures.saturating_sub(replenished);
        if failures == 0 {
            return Spent {
                failures,
                since: now,
            };
        }
        let advance = self.config.replenish.saturating_mul(replenished);
        let since = ChronoDuration::from_std(advance)
            .ok()
            .and_then(|advance| spent.since.checked_add_signed(advance))
            .unwrap_or(now);
        Spent { failures, since }
    }
}

fn throttle_keys(account: Option<&str>, ip: Option<IpAddr>) -> Vec<ThrottleKey> {
    let mut keys = Vec::with_capacity(2);
    if let Some(account) = account {
        keys.push(ThrottleKey::Account(account.to_owned()));
    }
    if let Some(ip) = ip {
        keys.push(ThrottleKey::Ip(ip));
    }
    keys
}
