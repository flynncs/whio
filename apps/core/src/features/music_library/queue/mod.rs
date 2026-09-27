mod service;

pub use service::{RestoreQueueError, RestoreQueueSave, RestoreQueueService};

pub const MAX_RESTORE_QUEUE_ENTRIES: usize = 1000;
