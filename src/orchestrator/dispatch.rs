use crate::config::schema::ServiceConfig;
use crate::domain::issue::Issue;
use crate::domain::state::OrchestratorState;
use crate::domain::workflow::WorkflowId;

/// Check if an issue is eligible to be dispatched.
pub fn is_eligible(
    issue: &Issue,
    state: &OrchestratorState,
    config: &ServiceConfig,
    workflow_id: &WorkflowId,
    global_max_agents: Option<usize>,
) -> bool {
    let state_key = workflow_id.state_key(&issue.identifier);

    // Not already running
    if state.is_running(&state_key) {
        return false;
    }
    // Not in retry backoff (unless ready)
    if state.is_in_retry(&state_key) {
        return false;
    }
    // Not already completed successfully
    if state.is_completed_successfully(&state_key) {
        return false;
    }
    // Under per-workflow concurrency limit
    if state.running_count_for_workflow(&workflow_id.0) >= config.agent.max_concurrent_agents {
        return false;
    }
    // Under global concurrency limit (if set)
    if let Some(global_max) = global_max_agents
        && state.running_count() >= global_max
    {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::time::Duration;

    fn test_issue(id: &str) -> Issue {
        Issue {
            identifier: id.to_string(),
            title: "Test".to_string(),
            description: None,
            status: "Todo".to_string(),
            priority: None,
            url: None,
            notion_page_id: None,
            blockers: vec![],
            source: "notion".to_string(),
            extra: HashMap::new(),
            comments: vec![],
            workflow_id: String::new(),
        }
    }

    fn test_config(max: usize) -> ServiceConfig {
        let mut config = ServiceConfig::default();
        config.agent.max_concurrent_agents = max;
        config
    }

    #[test]
    fn eligible_basic() {
        let state = OrchestratorState::new();
        let issue = test_issue("TASK-1");
        let wf = WorkflowId("bugs".to_string());
        assert!(is_eligible(&issue, &state, &test_config(5), &wf, None));
    }

    #[test]
    fn ineligible_already_running() {
        let state = OrchestratorState::new();
        let wf = WorkflowId("bugs".to_string());
        let issue = test_issue("TASK-1");
        state.start_session(&wf.state_key("TASK-1"), issue.clone(), Duration::from_secs(300), "bugs");
        assert!(!is_eligible(&issue, &state, &test_config(5), &wf, None));
    }

    #[test]
    fn per_workflow_concurrency_limit() {
        let state = OrchestratorState::new();
        let wf = WorkflowId("bugs".to_string());
        let config = test_config(2);

        // Fill workflow to capacity
        state.start_session(&wf.state_key("TASK-1"), test_issue("TASK-1"), Duration::from_secs(300), "bugs");
        state.start_session(&wf.state_key("TASK-2"), test_issue("TASK-2"), Duration::from_secs(300), "bugs");

        let issue3 = test_issue("TASK-3");
        assert!(!is_eligible(&issue3, &state, &config, &wf, None));
    }

    #[test]
    fn global_concurrency_limit() {
        let state = OrchestratorState::new();
        let wf_a = WorkflowId("bugs".to_string());
        let wf_b = WorkflowId("sentry".to_string());
        let config = test_config(5); // per-workflow limit is 5

        // Run 2 on workflow A
        state.start_session(&wf_a.state_key("TASK-1"), test_issue("TASK-1"), Duration::from_secs(300), "bugs");
        state.start_session(&wf_a.state_key("TASK-2"), test_issue("TASK-2"), Duration::from_secs(300), "bugs");

        // Workflow B has capacity (per-workflow), but global cap is 2
        let issue = test_issue("TASK-3");
        assert!(!is_eligible(&issue, &state, &config, &wf_b, Some(2)));

        // With global cap of 5, workflow B should be eligible
        assert!(is_eligible(&issue, &state, &config, &wf_b, Some(5)));
    }

    #[test]
    fn state_key_isolation_across_workflows() {
        let state = OrchestratorState::new();
        let wf_a = WorkflowId("bugs".to_string());
        let wf_b = WorkflowId("sentry".to_string());
        let config = test_config(5);

        // Same issue ID, different workflows — both should be eligible
        let issue = test_issue("TASK-1");
        state.start_session(&wf_a.state_key("TASK-1"), issue.clone(), Duration::from_secs(300), "bugs");

        // Workflow A: not eligible (already running)
        assert!(!is_eligible(&issue, &state, &config, &wf_a, None));
        // Workflow B: eligible (different composite key)
        assert!(is_eligible(&issue, &state, &config, &wf_b, None));
    }

    #[test]
    fn running_count_for_workflow_filters_correctly() {
        let state = OrchestratorState::new();
        let wf_a = WorkflowId("bugs".to_string());
        let wf_b = WorkflowId("sentry".to_string());

        state.start_session(&wf_a.state_key("TASK-1"), test_issue("TASK-1"), Duration::from_secs(300), "bugs");
        state.start_session(&wf_a.state_key("TASK-2"), test_issue("TASK-2"), Duration::from_secs(300), "bugs");
        state.start_session(&wf_b.state_key("TASK-3"), test_issue("TASK-3"), Duration::from_secs(300), "sentry");

        assert_eq!(state.running_count_for_workflow("bugs"), 2);
        assert_eq!(state.running_count_for_workflow("sentry"), 1);
        assert_eq!(state.running_count(), 3);
    }
}
