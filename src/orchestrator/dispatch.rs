use crate::config::schema::ServiceConfig;
use crate::domain::epic::EpicGraph;
use crate::domain::issue::Issue;
use crate::domain::state::OrchestratorState;
use crate::domain::workflow::WorkflowId;

/// Result of eligibility check, including epic-aware base_branch.
#[derive(Debug, Clone)]
pub struct DispatchDecision {
    pub eligible: bool,
    pub base_branch: String,
}

/// Check if an issue is eligible to be dispatched.
/// Returns a `DispatchDecision` with eligibility and the base branch for epic workflows.
pub fn check_eligible(
    issue: &Issue,
    state: &OrchestratorState,
    config: &ServiceConfig,
    workflow_id: &WorkflowId,
    global_max_agents: Option<usize>,
    epic_graph: Option<&EpicGraph>,
) -> DispatchDecision {
    let default_branch = &config.workspace.default_branch;
    let state_key = workflow_id.state_key(&issue.identifier);
    let not_eligible = DispatchDecision {
        eligible: false,
        base_branch: default_branch.clone(),
    };

    // Not already running
    if state.is_running(&state_key) {
        return not_eligible;
    }
    // Not in retry backoff (unless ready)
    if state.is_in_retry(&state_key) {
        return not_eligible;
    }
    // Not already completed successfully
    if state.is_completed_successfully(&state_key) {
        return not_eligible;
    }
    // Under per-workflow concurrency limit
    if state.running_count_for_workflow(&workflow_id.0) >= config.agent.max_concurrent_agents {
        return not_eligible;
    }
    // Under global concurrency limit (if set)
    if let Some(global_max) = global_max_agents
        && state.running_count() >= global_max
    {
        return not_eligible;
    }

    // Check blockers via epic graph if available, otherwise fall back to issue.blockers
    if let Some(graph) = epic_graph {
        let task_states = build_task_states(state, workflow_id);
        let eligibility =
            graph.check_eligibility(&issue.identifier, &task_states, default_branch);
        if !eligibility.eligible {
            tracing::debug!(
                issue_id = issue.identifier,
                unresolved = ?eligibility.unresolved,
                "epic: blocked by unresolved dependencies"
            );
            return not_eligible;
        }
        return DispatchDecision {
            eligible: true,
            base_branch: eligibility.base_branch,
        };
    }

    // No epic graph — use legacy blocker check
    if !issue.blockers.is_empty() {
        return not_eligible;
    }

    DispatchDecision {
        eligible: true,
        base_branch: default_branch.clone(),
    }
}

/// Backward-compatible wrapper for callers that don't need DispatchDecision.
pub fn is_eligible(
    issue: &Issue,
    state: &OrchestratorState,
    config: &ServiceConfig,
    workflow_id: &WorkflowId,
    global_max_agents: Option<usize>,
) -> bool {
    check_eligible(issue, state, config, workflow_id, global_max_agents, None).eligible
}

/// Build a map of task_identifier → TaskState from orchestrator state.
///
/// Filters to the given workflow so that identifiers from other workflows
/// don't leak into the epic dependency graph.
///
/// Priority ordering: HasPr > Completed > InProgress. A task with a tracked
/// PR is always reported as HasPr, even if a review worker is running on it.
fn build_task_states(
    state: &OrchestratorState,
    workflow_id: &WorkflowId,
) -> std::collections::HashMap<String, crate::domain::epic::TaskState> {
    use crate::domain::epic::TaskState;
    let mut task_states = std::collections::HashMap::new();

    // Snapshot everything under one lock to avoid state drift between calls.
    let snapshot = state.snapshot();

    // Tasks with open PRs → HasPr with branch name (highest priority)
    for pr in &snapshot.open_prs {
        if pr.workflow_id == workflow_id.0 {
            task_states.insert(
                pr.issue.identifier.clone(),
                TaskState::HasPr(pr.branch_name.clone()),
            );
        }
    }

    // Completed tasks (won't overwrite HasPr)
    for entry in &snapshot.completed {
        if entry.workflow_id == workflow_id.0 && entry.success {
            task_states
                .entry(entry.issue.identifier.clone())
                .or_insert(TaskState::Completed);
        }
    }

    // Running tasks (won't overwrite HasPr or Completed)
    for entry in &snapshot.running {
        if entry.workflow_id == workflow_id.0 {
            task_states
                .entry(entry.issue.identifier.clone())
                .or_insert(TaskState::InProgress);
        }
    }

    task_states
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

    #[test]
    fn build_task_states_running() {
        let state = OrchestratorState::new();
        let wf = WorkflowId("epic".to_string());
        state.start_session(
            &wf.state_key("TASK-1"),
            test_issue("TASK-1"),
            Duration::from_secs(300),
            "epic",
        );

        let ts = build_task_states(&state, &wf);
        assert_eq!(
            ts.get("TASK-1"),
            Some(&crate::domain::epic::TaskState::InProgress)
        );
    }

    #[test]
    fn build_task_states_has_pr() {
        let state = OrchestratorState::new();
        let wf = WorkflowId("epic".to_string());
        state.track_pr(
            &wf.state_key("TASK-1"),
            test_issue("TASK-1"),
            42,
            "/tmp".into(),
            "epic",
            "symposium/task-TASK-1",
        );

        let ts = build_task_states(&state, &wf);
        assert_eq!(
            ts.get("TASK-1"),
            Some(&crate::domain::epic::TaskState::HasPr(
                "symposium/task-TASK-1".to_string()
            ))
        );
    }

    #[test]
    fn build_task_states_completed() {
        let state = OrchestratorState::new();
        let wf = WorkflowId("epic".to_string());

        // Start and complete a session
        state.start_session(
            &wf.state_key("TASK-1"),
            test_issue("TASK-1"),
            Duration::from_secs(300),
            "epic",
        );
        state.mark_worker_done(&wf.state_key("TASK-1"), true, None);

        let ts = build_task_states(&state, &wf);
        assert_eq!(
            ts.get("TASK-1"),
            Some(&crate::domain::epic::TaskState::Completed)
        );
    }

    #[test]
    fn build_task_states_has_pr_takes_priority_over_completed() {
        let state = OrchestratorState::new();
        let wf = WorkflowId("epic".to_string());

        // Task completed successfully AND has a tracked PR
        state.start_session(
            &wf.state_key("TASK-1"),
            test_issue("TASK-1"),
            Duration::from_secs(300),
            "epic",
        );
        state.mark_worker_done(&wf.state_key("TASK-1"), true, None);
        state.track_pr(
            &wf.state_key("TASK-1"),
            test_issue("TASK-1"),
            42,
            "/tmp".into(),
            "epic",
            "symposium/task-TASK-1",
        );

        let ts = build_task_states(&state, &wf);
        // HasPr should take priority — downstream tasks can stack on this branch
        assert_eq!(
            ts.get("TASK-1"),
            Some(&crate::domain::epic::TaskState::HasPr(
                "symposium/task-TASK-1".to_string()
            ))
        );
    }

    #[test]
    fn build_task_states_filters_by_workflow() {
        let state = OrchestratorState::new();
        let wf_a = WorkflowId("epic-a".to_string());
        let wf_b = WorkflowId("epic-b".to_string());

        state.start_session(
            &wf_a.state_key("TASK-1"),
            test_issue("TASK-1"),
            Duration::from_secs(300),
            "epic-a",
        );
        state.track_pr(
            &wf_b.state_key("TASK-2"),
            test_issue("TASK-2"),
            99,
            "/tmp".into(),
            "epic-b",
            "symposium/task-TASK-2",
        );

        // Workflow A should only see its own running task
        let ts_a = build_task_states(&state, &wf_a);
        assert!(ts_a.contains_key("TASK-1"));
        assert!(!ts_a.contains_key("TASK-2"));

        // Workflow B should only see its own PR
        let ts_b = build_task_states(&state, &wf_b);
        assert!(!ts_b.contains_key("TASK-1"));
        assert!(ts_b.contains_key("TASK-2"));
    }

    #[test]
    fn epic_eligible_with_graph() {
        use crate::domain::epic::EpicGraph;
        use std::collections::HashSet;

        let state = OrchestratorState::new();
        let wf = WorkflowId("epic".to_string());
        let config = test_config(5);

        // Task-2 depends on Task-1; Task-1 has a PR
        let mut graph = EpicGraph::default();
        graph
            .dependencies
            .insert("TASK-1".to_string(), HashSet::new());
        graph.dependencies.insert(
            "TASK-2".to_string(),
            HashSet::from(["TASK-1".to_string()]),
        );

        state.track_pr(
            &wf.state_key("TASK-1"),
            test_issue("TASK-1"),
            1,
            "/tmp".into(),
            "epic",
            "symposium/task-TASK-1",
        );

        let decision =
            check_eligible(&test_issue("TASK-2"), &state, &config, &wf, None, Some(&graph));
        assert!(decision.eligible);
        assert_eq!(decision.base_branch, "symposium/task-TASK-1");
    }

    #[test]
    fn epic_blocked_when_dep_not_started() {
        use crate::domain::epic::EpicGraph;
        use std::collections::HashSet;

        let state = OrchestratorState::new();
        let wf = WorkflowId("epic".to_string());
        let config = test_config(5);

        let mut graph = EpicGraph::default();
        graph
            .dependencies
            .insert("TASK-1".to_string(), HashSet::new());
        graph.dependencies.insert(
            "TASK-2".to_string(),
            HashSet::from(["TASK-1".to_string()]),
        );
        // TASK-1 has no state → not resolved

        let decision =
            check_eligible(&test_issue("TASK-2"), &state, &config, &wf, None, Some(&graph));
        assert!(!decision.eligible);
    }
}
