use std::sync::Arc;

use chrono::{DateTime, Utc};

#[derive(Clone)]
pub struct Clock(Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>);

impl Clock {
    pub fn system() -> Self {
        Self(Arc::new(Utc::now))
    }

    pub fn from_fn(now: impl Fn() -> DateTime<Utc> + Send + Sync + 'static) -> Self {
        Self(Arc::new(now))
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        (self.0)()
    }
}
