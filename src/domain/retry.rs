use chrono::{DateTime, Utc};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct RetryEntry {
    pub issue_id: String,
    pub attempt: u32,
    pub next_retry_at: Option<DateTime<Utc>>,
    pub ready: bool,
}

impl RetryEntry {
    pub fn new(issue_id: String, attempt: u32) -> Self {
        Self {
            issue_id,
            attempt,
            next_retry_at: None,
            ready: false,
        }
    }

    /// Calculate exponential backoff: base_delay * 2^attempt, capped at max_delay.
    pub fn calculate_backoff(attempt: u32, base_delay: Duration, max_delay: Duration) -> Duration {
        let backoff = base_delay.saturating_mul(2u32.saturating_pow(attempt));
        if backoff > max_delay {
            max_delay
        } else {
            backoff
        }
    }
}
