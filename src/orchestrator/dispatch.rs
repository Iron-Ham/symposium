use crate::config::schema::ServiceConfig;
use crate::domain::issue::Issue;
use crate::domain::state::OrchestratorState;

/// Check if an issue is eligible to be dispatched.
pub fn is_eligible(issue: &Issue, state: &OrchestratorState, config: &ServiceConfig) -> bool {
    // Not already running
    if state.is_running(&issue.identifier) {
        return false;
    }
    // Not in retry backoff (unless ready)
    if state.is_in_retry(&issue.identifier) {
        return false;
    }
    // Not already completed successfully
    if state.is_completed_successfully(&issue.identifier) {
        return false;
    }
    // Under concurrency limit
    if state.running_count() >= config.agent.max_concurrent_agents {
        return false;
    }
    // Has no unresolved blockers
    if !issue.blockers.is_empty() {
        return false;
    }
    true
}

/// Sort candidate issues by priority (higher priority first).
/// Priority mapping: Urgent > High > Medium > Low > None
pub fn sort_candidates(issues: &mut [Issue]) {
    issues.sort_by(|a, b| {
        let pa = priority_rank(a.priority.as_deref());
        let pb = priority_rank(b.priority.as_deref());
        pb.cmp(&pa) // Descending: higher rank first
    });
}

fn priority_rank(priority: Option<&str>) -> u8 {
    match priority {
        Some("Urgent") => 5,
        Some("High") => 4,
        Some("Medium") => 3,
        Some("Low") => 2,
        Some(_) => 1,
        None => 0,
    }
}
