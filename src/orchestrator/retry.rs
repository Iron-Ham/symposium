use super::OrchestratorEvent;
use std::time::Duration;
use tokio::sync::mpsc;

/// Schedule a retry by spawning a delayed task that fires a RetryFired event.
pub fn schedule_retry(
    state_key: String,
    _attempt: u32,
    delay: Duration,
    workflow_id: String,
    event_tx: mpsc::Sender<OrchestratorEvent>,
) {
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        if let Err(e) = event_tx
            .send(OrchestratorEvent::RetryFired {
                state_key: state_key.clone(),
                workflow_id,
            })
            .await
        {
            tracing::error!(state_key, "failed to send RetryFired event: {e}");
        }
    });
}

/// Calculate exponential backoff delay: base * 2^attempt, capped at max.
pub fn calculate_backoff(attempt: u32, base: Duration, max: Duration) -> Duration {
    let delay = base.saturating_mul(2u32.saturating_pow(attempt));
    if delay > max { max } else { delay }
}
