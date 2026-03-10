use crate::domain::state::OrchestratorState;
use chrono::Utc;

/// Check for stalled workers (no activity within their per-entry stall_timeout).
///
/// Currently warn-only. We do NOT mark sessions as done here because the
/// spawned tokio task and agent process are still running — removing the
/// session from state without killing the process causes retry agents to
/// collide in the same worktree. The agent's own `turn_timeout_ms` is the
/// real safeguard against hangs.
pub fn check_stalled(state: &OrchestratorState) {
    let now = Utc::now();
    let stalled = state.find_stalled_sessions(now);

    for (state_key, stall_timeout) in stalled {
        tracing::warn!(
            state_key,
            stall_timeout_secs = stall_timeout.as_secs(),
            "session appears stalled (no events within timeout) — agent process still running"
        );
    }
}
