use super::OrchestratorEvent;
use std::time::Duration;
use tokio::sync::mpsc;

/// Schedule a retry by spawning a delayed task that fires a RetryFired event.
pub fn schedule_retry(
    issue_id: String,
    _attempt: u32,
    delay: Duration,
    event_tx: mpsc::Sender<OrchestratorEvent>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        let _ = event_tx.send(OrchestratorEvent::RetryFired { issue_id }).await;
    });
}

/// Calculate exponential backoff delay: base * 2^attempt, capped at max.
pub fn calculate_backoff(attempt: u32, base: Duration, max: Duration) -> Duration {
    let delay = base.saturating_mul(2u32.pow(attempt));
    if delay > max { max } else { delay }
}
