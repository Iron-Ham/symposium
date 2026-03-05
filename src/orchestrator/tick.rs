use crate::agent;
use crate::agent::worker::run_agent_attempt;
use crate::config::schema::ServiceConfig;
use crate::domain::issue::Issue;
use crate::domain::retry::RetryEntry;
use crate::domain::state::OrchestratorState;
use crate::error::Result;
use crate::prompt;
use crate::tracker::notion::NotionTracker;
use crate::tracker::TrackerClient;
use crate::workspace::WorkspaceManager;

use super::dispatch;
use super::reconcile;
use super::OrchestratorEvent;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Execute one poll-and-dispatch cycle.
pub async fn run_tick(
    state: &OrchestratorState,
    config_rx: &watch::Receiver<ServiceConfig>,
    event_tx: &mpsc::Sender<OrchestratorEvent>,
) -> Result<()> {
    let config = config_rx.borrow().clone();

    // 1. Reconcile: check for stalled workers
    reconcile::check_stalled(state, &config);

    // 2. Dispatch ready retries
    let ready_retries = state.take_ready_retries();
    for retry in &ready_retries {
        tracing::info!(issue_id = %retry.issue_id, attempt = retry.attempt, "retry ready");
    }

    // 3. Check if we have capacity for new work
    let running = state.running_count();
    let max = config.agent.max_concurrent_agents;
    tracing::debug!(running, max, retries = ready_retries.len(), "tick");

    if running >= max && ready_retries.is_empty() {
        return Ok(());
    }

    // 4. Connect to tracker and fetch candidates
    let mut tracker = match NotionTracker::new(config.tracker.clone()).await {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("failed to connect to tracker: {e}");
            return Ok(());
        }
    };

    let mut candidates = match tracker.fetch_candidate_issues().await {
        Ok(issues) => issues,
        Err(e) => {
            tracing::error!("failed to fetch candidates: {e}");
            return Ok(());
        }
    };

    // 5. Sort by priority and filter eligible
    dispatch::sort_candidates(&mut candidates);

    // 6. Dispatch eligible issues (up to remaining capacity)
    for issue in candidates {
        if !dispatch::is_eligible(&issue, state, &config) {
            continue;
        }

        dispatch_issue(issue, state, &config, config_rx, event_tx);
    }

    // 7. Re-dispatch ready retries
    for retry in ready_retries {
        dispatch_retry(retry, state, &config, config_rx, event_tx);
    }

    // 8. Clean up workspaces for terminal issues
    cleanup_terminal(&mut tracker, state, config_rx).await;

    Ok(())
}

/// Spawn a worker task for a new issue.
fn dispatch_issue(
    issue: Issue,
    state: &OrchestratorState,
    config: &ServiceConfig,
    config_rx: &watch::Receiver<ServiceConfig>,
    event_tx: &mpsc::Sender<OrchestratorEvent>,
) {
    let issue_id = issue.identifier.clone();
    tracing::info!(issue_id, "dispatching worker");

    state.start_session(issue.clone());

    let config = config.clone();
    let config_rx = config_rx.clone();
    let event_tx = event_tx.clone();

    let state_clone = state.clone();
    tokio::spawn(async move {
        let result = run_worker(&issue, &config, &config_rx, None, &state_clone).await;

        let (success, error) = match &result {
            Ok(true) => (true, None),
            Ok(false) => (false, Some("max turns reached".to_string())),
            Err(e) => (false, Some(e.to_string())),
        };

        let _ = event_tx
            .send(OrchestratorEvent::WorkerCompleted {
                issue_id: issue.identifier.clone(),
                success,
                error: error.clone(),
            })
            .await;

        // Schedule retry on failure
        if !success {
            let base = Duration::from_secs(1);
            let max = Duration::from_secs(300);
            let delay = super::retry::calculate_backoff(0, base, max);
            super::retry::schedule_retry(issue.identifier, 1, delay, event_tx);
        }
    });
}

/// Spawn a worker task for a retry.
fn dispatch_retry(
    retry: RetryEntry,
    state: &OrchestratorState,
    config: &ServiceConfig,
    config_rx: &watch::Receiver<ServiceConfig>,
    event_tx: &mpsc::Sender<OrchestratorEvent>,
) {
    let issue_id = retry.issue_id.clone();

    // We need the issue data — check if we still have it in completed or re-fetch
    // For retries, we'll create a minimal issue from the retry entry
    tracing::info!(issue_id, attempt = retry.attempt, "dispatching retry");

    let config = config.clone();
    let config_rx = config_rx.clone();
    let event_tx = event_tx.clone();
    let state_clone = state.clone();
    let attempt = retry.attempt;

    tokio::spawn(async move {
        // Re-fetch the issue from the tracker for retry
        let mut tracker = match NotionTracker::new(config.tracker.clone()).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(issue_id, "retry: failed to connect to tracker: {e}");
                return;
            }
        };

        let issues = match tracker.fetch_issue_states_by_ids(std::slice::from_ref(&issue_id)).await {
            Ok(issues) => issues,
            Err(e) => {
                tracing::error!(issue_id, "retry: failed to fetch issue: {e}");
                return;
            }
        };

        let Some(issue) = issues.into_iter().next() else {
            tracing::warn!(issue_id, "retry: issue not found in tracker");
            return;
        };

        let result = run_worker(&issue, &config, &config_rx, Some(attempt), &state_clone).await;

        let (success, error) = match &result {
            Ok(true) => (true, None),
            Ok(false) => (false, Some("max turns reached".to_string())),
            Err(e) => (false, Some(e.to_string())),
        };

        let _ = event_tx
            .send(OrchestratorEvent::WorkerCompleted {
                issue_id: issue.identifier.clone(),
                success,
                error: error.clone(),
            })
            .await;

        // Schedule further retry on failure, with increasing backoff
        if !success {
            let base = Duration::from_secs(1);
            let max = Duration::from_secs(300);
            let delay = super::retry::calculate_backoff(attempt, base, max);
            super::retry::schedule_retry(issue.identifier, attempt + 1, delay, event_tx);
        }
    });
}

/// Run a worker: prepare workspace, build prompt, start agent, run turns.
async fn run_worker(
    issue: &Issue,
    config: &ServiceConfig,
    config_rx: &watch::Receiver<ServiceConfig>,
    attempt: Option<u32>,
    state: &OrchestratorState,
) -> Result<bool> {
    use crate::domain::session::{AgentEvent, AgentEventKind, RunStatus};
    use crate::workspace::hooks;

    let ws = WorkspaceManager::new(config_rx.clone());

    // Ensure workspace exists (creates + runs after_create hook if new)
    state.push_agent_event(
        &issue.identifier,
        AgentEvent::now(AgentEventKind::Status {
            status: "Setting up workspace".into(),
        }),
    );
    let workspace_dir = ws.ensure(issue).await?;

    // Run before_run hook
    ws.prepare(issue, attempt).await?;

    // Build prompt from template
    let prompt_text = prompt::build_prompt(&config.prompt_template, issue, attempt)?;

    // Start agent session
    state.push_agent_event(
        &issue.identifier,
        AgentEvent::now(AgentEventKind::Status {
            status: "Starting agent".into(),
        }),
    );
    state.update_session_status(&issue.identifier, RunStatus::Running);

    let runner = agent::AgentRunner::new(config.clone());
    let mut worker = runner
        .start_session(&workspace_dir, &prompt_text, &issue.identifier)
        .await?;

    // Run multi-turn agent loop
    let success =
        run_agent_attempt(&mut worker, &prompt_text, state, &issue.identifier).await?;

    if success {
        // Post-completion pipeline: commit → review → PR
        let hook_timeout = config.hooks.timeout();

        // 1. Commit the agent's changes
        state.push_agent_event(
            &issue.identifier,
            AgentEvent::now(AgentEventKind::Status {
                status: "Committing agent changes".into(),
            }),
        );
        let title_safe = issue.title.replace('\'', "'\\''");
        let commit_script = format!(
            "git add -A && git diff --cached --quiet || git commit -m 'fix: {}'",
            title_safe,
        );
        if let Err(e) = hooks::run_hook(&commit_script, &workspace_dir, hook_timeout).await {
            tracing::warn!(issue_id = issue.identifier, "commit hook failed: {e}");
        }

        // 2. Run review agent
        state.push_agent_event(
            &issue.identifier,
            AgentEvent::now(AgentEventKind::Status {
                status: "Running deep review".into(),
            }),
        );
        let review_prompt = build_review_prompt(issue);
        match runner
            .start_session(&workspace_dir, &review_prompt, &issue.identifier)
            .await
        {
            Ok(mut review_worker) => {
                match run_agent_attempt(
                    &mut review_worker,
                    &review_prompt,
                    state,
                    &issue.identifier,
                )
                .await
                {
                    Ok(_) => {
                        // Commit review fixes if any
                        let review_commit =
                            "git add -A && git diff --cached --quiet || git commit -m 'review: address code review feedback'";
                        if let Err(e) =
                            hooks::run_hook(review_commit, &workspace_dir, hook_timeout).await
                        {
                            tracing::warn!(
                                issue_id = issue.identifier,
                                "review commit failed: {e}"
                            );
                        }
                    }
                    Err(e) => {
                        tracing::warn!(
                            issue_id = issue.identifier,
                            "review agent failed: {e}"
                        );
                    }
                }
            }
            Err(e) => {
                tracing::warn!(
                    issue_id = issue.identifier,
                    "failed to start review agent: {e}"
                );
            }
        }

        // 3. Push branch and open draft PR
        state.push_agent_event(
            &issue.identifier,
            AgentEvent::now(AgentEventKind::Status {
                status: "Opening draft PR".into(),
            }),
        );
        let pr_title = format!("[BUG-{}] {}", issue.identifier, issue.title)
            .replace('\'', "'\\''");
        let pr_body_text = format!(
            "Automated fix for bug **{}**: {}\n\n---\n*Opened by Symposium*",
            issue.identifier, issue.title,
        )
        .replace('\'', "'\\''");
        let pr_script = format!(
            "git push -u origin HEAD 2>&1 && gh pr create --draft --title '{}' --body '{}' 2>&1 || true",
            pr_title, pr_body_text,
        );
        match hooks::run_hook(&pr_script, &workspace_dir, hook_timeout).await {
            Ok(()) => {
                tracing::info!(issue_id = issue.identifier, "draft PR created");
                state.push_agent_event(
                    &issue.identifier,
                    AgentEvent::now(AgentEventKind::Status {
                        status: "Draft PR opened".into(),
                    }),
                );
            }
            Err(e) => {
                tracing::warn!(issue_id = issue.identifier, "PR creation failed: {e}");
                state.push_agent_event(
                    &issue.identifier,
                    AgentEvent::now(AgentEventKind::Error {
                        message: format!("PR creation failed: {e}"),
                    }),
                );
            }
        }
    }

    // Run after_run hook
    ws.finish(issue, success).await?;

    Ok(success)
}

/// Build a review-focused prompt for the second agent pass.
fn build_review_prompt(issue: &Issue) -> String {
    format!(
        r#"You are reviewing changes for bug {id}: {title}.

First, read `CLAUDE.md` at the repo root and any relevant subsystem `AGENTS.md` files.

Then run `git diff origin/main` to see all changes on this branch.

Perform a thorough code review covering:

1. **Correctness** — Does the fix actually address the bug? Are there edge cases or off-by-one errors?
2. **Error handling** — Are errors handled properly? No swallowed errors or missing error paths?
3. **Type safety** — Are types used correctly? Any unsafe casts or implicit conversions?
4. **Performance** — Any unnecessary allocations, N+1 queries, or hot-path regressions?
5. **Security** — Any injection, XSS, auth bypass, or other vulnerabilities introduced?
6. **Tests** — Are tests adequate? Do they cover the regression? Are there missing test cases?
7. **Code quality** — Naming, duplication, dead code, or overly complex logic?

Fix any real issues you find. Keep changes minimal — only fix actual problems, do not refactor working code or make stylistic changes. If no issues are found, do nothing."#,
        id = issue.identifier,
        title = issue.title,
    )
}


/// Clean up workspaces for issues that have reached terminal states.
async fn cleanup_terminal(
    tracker: &mut NotionTracker,
    state: &OrchestratorState,
    config_rx: &watch::Receiver<ServiceConfig>,
) {
    let terminal_issues = match tracker.fetch_terminal_issues().await {
        Ok(issues) => issues,
        Err(e) => {
            tracing::debug!("failed to fetch terminal issues for cleanup: {e}");
            return;
        }
    };

    let ws = WorkspaceManager::new(config_rx.clone());
    for issue in terminal_issues {
        if !state.is_running(&issue.identifier)
            && let Err(e) = ws.remove(&issue.identifier).await {
                tracing::warn!(
                    issue_id = issue.identifier,
                    "failed to remove terminal workspace: {e}"
                );
            }
    }
}
