use crate::config::schema::ServiceConfig;
use crate::domain::state::OrchestratorState;
use chrono::Utc;

/// Check for stalled workers (no activity within stall_timeout).
///
/// Currently warn-only. We do NOT mark sessions as done here because the
/// spawned tokio task and agent process are still running — removing the
/// session from state without killing the process causes retry agents to
/// collide in the same worktree. The agent's own `turn_timeout_ms` is the
/// real safeguard against hangs.
pub fn check_stalled(state: &OrchestratorState, config: &ServiceConfig) {
    let stall_timeout = config.codex.stall_timeout();
    let now = Utc::now();
    let stalled = state.find_stalled_sessions(now, stall_timeout);

    for issue_id in stalled {
        tracing::warn!(
            issue_id,
            stall_timeout_secs = stall_timeout.as_secs(),
            "session appears stalled (no events within timeout) — agent process still running"
        );
    }
}
