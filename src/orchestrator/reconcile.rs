use crate::domain::issue::Issue;
use crate::domain::state::OrchestratorState;
use crate::domain::workflow::WorkflowHandle;
use crate::workspace::hooks;
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

const GH_TIMEOUT: Duration = Duration::from_secs(15);

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

/// Scan existing workspaces for open PRs and register them for review monitoring.
///
/// This handles the bootstrap problem: PRs created before persistence was added
/// (or if the state file was lost) are rediscovered from the filesystem. For each
/// workflow with `pr_review.enabled`, we enumerate workspace directories, run
/// `gh pr view` in each, and track any OPEN PRs we find.
pub async fn discover_open_prs(state: &OrchestratorState, workflows: &[WorkflowHandle]) {
    let tracked: std::collections::HashSet<String> = state
        .open_prs()
        .iter()
        .map(|pr| pr.issue.identifier.clone())
        .collect();

    for wf in workflows {
        let config = wf.config_rx.borrow().clone();
        if !config.pr_review.enabled {
            continue;
        }

        let root = PathBuf::from(&config.workspace.root);
        if !root.exists() {
            continue;
        }

        let dirs = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) => {
                tracing::warn!(
                    workflow = %wf.id,
                    path = %root.display(),
                    "failed to read workspace root for PR discovery: {e}"
                );
                continue;
            }
        };

        for entry in dirs.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(issue_id) = entry.file_name().to_str().map(|s| s.to_string()) else {
                continue;
            };

            // Skip if already tracked
            if tracked.contains(&issue_id) {
                continue;
            }

            // Run gh pr view to check for an open PR
            let script = "gh pr view --json number,state,title,headRefName 2>/dev/null";
            let output = match hooks::run_hook_with_output(script, &path, GH_TIMEOUT).await {
                Ok(o) => o,
                Err(_) => continue, // no PR on this branch, or gh not available
            };

            let data: serde_json::Value = match serde_json::from_str(output.trim()) {
                Ok(d) => d,
                Err(_) => continue,
            };

            let pr_state = data["state"].as_str().unwrap_or("");
            if pr_state != "OPEN" {
                continue;
            }

            let Some(pr_number) = data["number"].as_u64() else {
                continue;
            };
            let title = data["title"]
                .as_str()
                .unwrap_or("(unknown)")
                .to_string();

            let state_key = wf.id.state_key(&issue_id);
            let issue = Issue {
                identifier: issue_id.clone(),
                title,
                description: None,
                status: String::new(),
                priority: None,
                url: None,
                notion_page_id: None,
                blockers: vec![],
                source: "reconciled".to_string(),
                extra: HashMap::new(),
                comments: vec![],
                workflow_id: wf.id.0.clone(),
            };

            let branch_name = data["headRefName"]
                .as_str()
                .unwrap_or("")
                .to_string();
            state.track_pr(
                &state_key,
                issue,
                pr_number,
                path,
                &wf.id.0,
                &branch_name,
            );
            tracing::info!(
                workflow = %wf.id,
                issue_id,
                pr = pr_number,
                "discovered open PR from existing workspace"
            );
        }
    }
}
