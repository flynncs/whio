use super::{ValidationError, text::optional};

const MAX_ERROR: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderEntityState {
    fetched_at: chrono::DateTime<chrono::Utc>,
    reported_count: Option<u32>,
    last_error: Option<String>,
}

impl ProviderEntityState {
    pub fn new(
        fetched_at: chrono::DateTime<chrono::Utc>,
        reported_count: Option<u32>,
        last_error: Option<String>,
    ) -> Result<Self, ValidationError> {
        Ok(Self {
            fetched_at,
            reported_count,
            last_error: optional("last_error", last_error, MAX_ERROR)?,
        })
    }

    pub fn fetched_at(&self) -> chrono::DateTime<chrono::Utc> {
        self.fetched_at
    }

    pub fn reported_count(&self) -> Option<u32> {
        self.reported_count
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    pub fn is_partial(&self, member_count: u32) -> bool {
        self.reported_count
            .is_some_and(|reported| member_count < reported)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(reported_count: Option<u32>, last_error: Option<&str>) -> ProviderEntityState {
        ProviderEntityState::new(
            "2026-09-21T00:00:00Z".parse().unwrap(),
            reported_count,
            last_error.map(str::to_owned),
        )
        .unwrap()
    }

    #[test]
    fn partial_is_members_below_a_known_reported_count() {
        assert!(state(Some(12), None).is_partial(11));
        assert!(!state(Some(12), None).is_partial(12));
        assert!(!state(Some(12), None).is_partial(13));
        assert!(!state(None, None).is_partial(0));
    }

    #[test]
    fn a_blank_last_error_is_absent() {
        assert_eq!(state(None, Some("   ")).last_error(), None);
    }
}
