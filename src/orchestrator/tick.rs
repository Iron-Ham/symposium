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

    // Guard: don't dispatch a retry if the issue is still running (e.g. stale retry entry)
    if state.is_running(&issue_id) {
        tracing::warn!(issue_id, "skipping retry — session still running");
        return;
    }

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

    // Build prompt from template, with PR metadata instructions appended.
    // The implementer has the best context for writing the initial PR description
    // since it performed the investigation and chose the fix.
    let mut prompt_text = prompt::build_prompt(&config.prompt_template, issue, attempt)?;
    prompt_text.push_str(&pr_metadata_instructions(issue));

    // Start agent session
    state.push_agent_event(
        &issue.identifier,
        AgentEvent::now(AgentEventKind::Status {
            status: "Starting agent".into(),
        }),
    );
    state.update_session_status(&issue.identifier, RunStatus::Running);

    // Use agent_subdirectory if configured (e.g. "mail-ios" within the repo worktree)
    let agent_dir = match &config.workspace.agent_subdirectory {
        Some(sub) => workspace_dir.join(sub),
        None => workspace_dir.clone(),
    };

    let runner = agent::AgentRunner::new(config.clone());
    let (mut worker, _mcp_guard) = runner
        .start_session(&agent_dir, &prompt_text, &issue.identifier)
        .await?;

    // Run multi-turn agent loop
    let success =
        run_agent_attempt(&mut worker, &prompt_text, state, &issue.identifier).await?;

    if success {
        // Post-completion pipeline: commit → review → PR
        let hook_timeout = config.hooks.timeout();

        // 1. Run review agent (if enabled)
        if config.review.enabled {
            state.push_agent_event(
                &issue.identifier,
                AgentEvent::now(AgentEventKind::Status {
                    status: "Running deep review".into(),
                }),
            );

            // Run before_review hook if configured
            if let Some(ref hook_script) = config.review.before_review {
                let rendered = prompt::build_prompt_with_workspace(
                    hook_script,
                    issue,
                    attempt,
                    Some(&workspace_dir.to_string_lossy()),
                )
                .unwrap_or_else(|_| hook_script.clone());
                if let Err(e) =
                    hooks::run_hook(&rendered, &workspace_dir, hook_timeout).await
                {
                    tracing::warn!(
                        issue_id = issue.identifier,
                        "before_review hook failed: {e}"
                    );
                }
            }

            let mut review_prompt = build_review_prompt(issue, &config.review.prompt_template);
            // Ask the reviewer to update the PR metadata the implementer wrote,
            // accounting for any changes the review introduced.
            review_prompt.push_str(&pr_metadata_update_instructions());
            match runner
                .start_session(&agent_dir, &review_prompt, &issue.identifier)
                .await
            {
                Ok((mut review_worker, _review_mcp_guard)) => {
                    match run_agent_attempt(
                        &mut review_worker,
                        &review_prompt,
                        state,
                        &issue.identifier,
                    )
                    .await
                    {
                        Ok(_) => {}
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
        }

        // 3. Push branch and open draft PR
        state.push_agent_event(
            &issue.identifier,
            AgentEvent::now(AgentEventKind::Status {
                status: "Opening draft PR".into(),
            }),
        );

        // Read agent-generated PR metadata, falling back to defaults
        let (pr_title, pr_body_text) =
            read_pr_metadata(&workspace_dir, issue).await;

        // Write title/body to temp files outside the workspace to avoid accidental git add
        let tmp = std::env::temp_dir();
        let title_file = tmp.join(format!("symposium-pr-title-{}", issue.identifier));
        let body_file = tmp.join(format!("symposium-pr-body-{}", issue.identifier));
        let _ = tokio::fs::write(&title_file, &pr_title).await;
        let _ = tokio::fs::write(&body_file, &pr_body_text).await;
        let pr_script = format!(
            "git push -u origin HEAD 2>&1 && gh pr create --draft --title \"$(cat {})\" --body-file {} 2>&1",
            title_file.display(),
            body_file.display(),
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
        // Clean up temp files
        let _ = tokio::fs::remove_file(&title_file).await;
        let _ = tokio::fs::remove_file(&body_file).await;
    }

    // Run after_run hook
    ws.finish(issue, success).await?;

    Ok(success)
}

/// Read agent-generated PR metadata from the workspace, falling back to defaults.
///
/// The agent is instructed to write `PR_TITLE` and `PR_BODY.md` in the workspace root.
/// These files contain the PR title (a single line) and the full PR body (markdown)
/// respectively. If either file is missing, we fall back to a generic default.
async fn read_pr_metadata(workspace_dir: &std::path::Path, issue: &Issue) -> (String, String) {
    let title_path = workspace_dir.join("PR_TITLE");
    let body_path = workspace_dir.join("PR_BODY.md");

    let pr_title = match tokio::fs::read_to_string(&title_path).await {
        Ok(contents) => {
            let _ = tokio::fs::remove_file(&title_path).await;
            let title = contents.trim().to_string();
            if title.is_empty() { default_pr_title(issue) } else { title }
        }
        Err(_) => default_pr_title(issue),
    };

    let pr_body = match tokio::fs::read_to_string(&body_path).await {
        Ok(contents) => {
            let _ = tokio::fs::remove_file(&body_path).await;
            let body = contents.trim().to_string();
            if body.is_empty() { default_pr_body(issue) } else { body }
        }
        Err(_) => default_pr_body(issue),
    };

    (pr_title, pr_body)
}

fn default_pr_title(issue: &Issue) -> String {
    format!("[{}] {}", issue.identifier, issue.title)
}

fn default_pr_body(issue: &Issue) -> String {
    format!(
        "Automated fix for **{}**: {}\n\n---\n*Opened by Symposium*",
        issue.identifier, issue.title,
    )
}

/// Build a review-focused prompt for the second agent pass.
///
/// If the user provides a custom `review.prompt_template` in their workflow config,
/// it is rendered as a Liquid template with issue variables. Otherwise, the built-in
/// default review prompt is used.
fn build_review_prompt(issue: &Issue, custom_template: &str) -> String {
    if !custom_template.is_empty() {
        return prompt::build_prompt(custom_template, issue, None)
            .unwrap_or_else(|e| {
                tracing::warn!("failed to render custom review template: {e}, using default");
                build_default_review_prompt(issue)
            });
    }
    build_default_review_prompt(issue)
}

fn build_default_review_prompt(issue: &Issue) -> String {
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

Fix any real issues you find. Keep changes minimal — only fix actual problems, do not refactor working code or make stylistic changes. If no issues are found, do nothing.

If you made any changes, commit them with `git add` and `git commit` with a descriptive message."#,
        id = issue.identifier,
        title = issue.title,
    )
}

/// Instructions appended to the implementer prompt for writing PR metadata files.
///
/// The implementer has the richest context — it investigated the bug, found
/// the root cause, and chose the fix — so it writes the initial PR description.
fn pr_metadata_instructions(issue: &Issue) -> String {
    format!(
        r#"

After committing, write a PR title and body for the changes on this branch:

1. Write a single-line PR title to a file called `PR_TITLE` in the root of the git repository. The title should be a concise, human-readable summary of the actual change (not just the bug title). Do NOT include prefixes like `fix:` or `[BUG-123]`.
2. Write a PR body in Markdown to a file called `PR_BODY.md` in the root of the git repository. The body should include:
   - **Summary**: 1-2 sentence overview of the change
   - **Investigation**: What you found when investigating the root cause
   - **Fix**: What was changed and why this approach was chosen
   - **Testing**: How the fix was verified
   - A link back to the issue: `Fixes {id}`

These files must NOT be git-committed — just write them to disk."#,
        id = issue.identifier,
    )
}

/// Instructions appended to the review prompt for updating PR metadata.
///
/// The reviewer may have made additional changes, so it updates the existing
/// PR metadata the implementer wrote to reflect the final state of the branch.
fn pr_metadata_update_instructions() -> String {
    r#"

Finally, read the `PR_TITLE` and `PR_BODY.md` files in the root of the git repository. These were written by the implementer. If you made any changes during your review, update these files to reflect the final state of the branch. If you made no changes, leave them as-is. Do NOT git-commit these files."#
        .to_string()
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
