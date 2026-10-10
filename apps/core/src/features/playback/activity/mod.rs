mod service;
#[cfg(test)]
mod tests;

pub use service::{ActivityError, PlaybackActivityService};

pub const SESSION_IDLE_TTL_MS: u64 = 30 * 60 * 1_000;
pub const QUALIFICATION_LIMIT_MS: u64 = 4 * 60 * 1_000;
pub const MAX_LIVE_SESSIONS_PER_PRINCIPAL: usize = 16;
