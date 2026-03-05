use crate::config::schema::ServiceConfig;
use crate::domain::state::OrchestratorState;
use chrono::Utc;

/// Check for stalled workers (no activity within stall_timeout).
pub fn check_stalled(state: &OrchestratorState, config: &ServiceConfig) {
    let stall_timeout = config.codex.stall_timeout();
    let now = Utc::now();
    let stalled = state.find_stalled_sessions(now, stall_timeout);

    for issue_id in stalled {
        tracing::warn!(issue_id, "session stalled, marking for retry");
        state.mark_worker_done(&issue_id, false, Some("stall timeout".into()));
    }
}
