use crate::agent;
use crate::agent::worker::run_agent_attempt;
use crate::config::schema::ServiceConfig;
use crate::domain::issue::Issue;
use crate::domain::retry::RetryEntry;
use crate::domain::state::OrchestratorState;
use crate::domain::workflow::{WorkflowHandle, WorkflowId};
use crate::error::Result;
use crate::prompt;
use crate::tracker::notion::NotionTracker;
use crate::tracker::sentry::SentryTracker;
use crate::tracker::TrackerClient;
use crate::workspace::hooks as workspace_hooks;
use crate::workspace::WorkspaceManager;

use super::dispatch;
use super::pr_review;
use super::OrchestratorEvent;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

/// Execute one poll-and-dispatch cycle for a single workflow.
pub async fn run_workflow_tick(
    workflow: &WorkflowHandle,
    state: &OrchestratorState,
    event_tx: &mpsc::Sender<OrchestratorEvent>,
    global_max_agents: Option<usize>,
) -> Result<()> {
    let config = workflow.config_rx.borrow().clone();
    let wf_id = &workflow.id;

    // 1. Dispatch ready retries for this workflow
    let ready_retries = state.take_ready_retries_for_workflow(&wf_id.0);
    for retry in &ready_retries {
        tracing::info!(
            state_key = %retry.issue_id,
            workflow = %wf_id,
            attempt = retry.attempt,
            "retry ready"
        );
    }

    // 2. Check if we have capacity for new work
    let running = state.running_count_for_workflow(&wf_id.0);
    let max = config.agent.max_concurrent_agents;
    tracing::debug!(
        workflow = %wf_id,
        running,
        max,
        retries = ready_retries.len(),
        "tick"
    );

    let at_workflow_capacity = running >= max;
    let at_global_capacity = global_max_agents
        .map(|g| state.running_count() >= g)
        .unwrap_or(false);

    let has_capacity =
        !(at_workflow_capacity || at_global_capacity) || !ready_retries.is_empty();

    // 3-7: Fetch candidates, dispatch workers, and clean up terminal issues.
    // Gated behind capacity check — but PR review checks (step 8) always run.
    if has_capacity {
        // 3. Connect to Notion tracker and fetch candidates (skip if no active states)
        let has_notion = !config.tracker.active_states.is_empty();
        let mut tracker: Option<NotionTracker> = None;
        let mut candidates: Vec<Issue> = Vec::new();

        if has_notion {
            match NotionTracker::new(config.tracker.clone()).await {
                Ok(t) => {
                    tracker = Some(t);
                }
                Err(e) => {
                    tracing::error!(workflow = %wf_id, "failed to connect to tracker: {e}");
                    return Ok(());
                }
            };

            match tracker.as_mut().unwrap().fetch_candidate_issues().await {
                Ok(issues) => {
                    tracing::info!(
                        workflow = %wf_id,
                        database_id = %config.tracker.database_id,
                        count = issues.len(),
                        "fetched Notion candidates"
                    );
                    candidates = issues;
                }
                Err(e) => {
                    tracing::error!(workflow = %wf_id, "failed to fetch candidates: {e}");
                    return Ok(());
                }
            }

            // Tag all candidates with the workflow ID
            for issue in &mut candidates {
                issue.workflow_id = wf_id.0.clone();
            }
        } else {
            tracing::debug!(workflow = %wf_id, "skipping Notion tracker (no active_states)");
        }

        // 3b. Fetch from Sentry (if enabled)
        let mut sentry_tracker: Option<SentryTracker> = None;
        if config.sentry.enabled {
            match SentryTracker::new(config.sentry.clone()).await {
                Ok(mut sentry) => match sentry.fetch_candidate_issues().await {
                    Ok(mut sentry_issues) => {
                        tracing::info!(
                            workflow = %wf_id,
                            project = %config.sentry.project,
                            count = sentry_issues.len(),
                            "fetched Sentry candidates"
                        );
                        for issue in &mut sentry_issues {
                            issue.workflow_id = wf_id.0.clone();
                        }
                        candidates.extend(sentry_issues);
                        sentry_tracker = Some(sentry);
                    }
                    Err(e) => tracing::error!(workflow = %wf_id, "failed to fetch Sentry issues: {e}"),
                },
                Err(e) => tracing::error!(workflow = %wf_id, "failed to create Sentry tracker: {e}"),
            }
        }

        // 4. Build epic graph if parent_page_id is set and graph not yet initialized.
        // Note: the graph is built once and cached for the lifetime of the process.
        // If tasks are added to the epic or Blocked by relations change after startup,
        // a restart is required to pick up the changes. New tasks that appear in
        // candidates but have no graph entry will be treated as having no dependencies.
        if config.tracker.parent_page_id.is_some()
            && state.epic_graph().is_none()
            && let Some(ref mut t) = tracker
        {
            match t.fetch_all_epic_tasks().await {
                Ok(all_tasks) => {
                    tracing::info!(
                        workflow = %wf_id,
                        count = all_tasks.len(),
                        "fetched all epic sub-tasks for dependency graph"
                    );
                    // For now, build graph from Blocked by relations only.
                    // Mermaid parsing requires fetching the epic page content,
                    // which can be added later.
                    let mut graph = crate::domain::epic::EpicGraph::default();
                    for task in &all_tasks {
                        graph
                            .dependencies
                            .entry(task.identifier.clone())
                            .or_default();
                        // Merge Blocked by relation edges from extra properties
                        if let Some(blocked_by) = task.extra.get("blocked by") {
                            let blocker_ids: Vec<String> = blocked_by
                                .split(',')
                                .map(|s| s.trim().to_string())
                                .filter(|s| !s.is_empty())
                                .collect();
                            graph.merge_blocked_by(&task.identifier, &blocker_ids);
                        }
                    }
                    state.set_epic_graph(graph);
                }
                Err(e) => {
                    tracing::error!(
                        workflow = %wf_id,
                        "failed to fetch epic sub-tasks: {e}"
                    );
                }
            }
        }

        // 4b. Sort by priority and filter eligible
        dispatch::sort_candidates(&mut candidates);
        let epic_graph = state.epic_graph();

        // Guard: if parent_page_id is configured but the graph isn't available,
        // skip dispatching to avoid sending all tasks simultaneously without
        // dependency ordering. The graph will be retried on the next tick.
        let skip_epic_dispatch =
            config.tracker.parent_page_id.is_some() && epic_graph.is_none();
        if skip_epic_dispatch {
            tracing::warn!(
                workflow = %wf_id,
                candidates = candidates.len(),
                "epic graph not available — skipping dispatch to preserve dependency ordering"
            );
        }

        // 5. Dispatch eligible issues (up to remaining capacity)
        if !skip_epic_dispatch {
            for issue in candidates {
                let decision = dispatch::check_eligible(
                    &issue,
                    state,
                    &config,
                    wf_id,
                    global_max_agents,
                    epic_graph.as_ref(),
                );
                if !decision.eligible {
                    continue;
                }

                dispatch_issue(
                    issue,
                    state,
                    &config,
                    &workflow.config_rx,
                    event_tx,
                    wf_id,
                    &decision.base_branch,
                );
            }
        }

        // 6. Re-dispatch ready retries
        for retry in ready_retries {
            dispatch_retry(retry, state, &config, &workflow.config_rx, event_tx, wf_id);
        }

        // 7. Clean up workspaces for terminal issues (only if Notion tracker is active)
        if let Some(ref mut t) = tracker {
            cleanup_terminal(t, state, &workflow.config_rx, wf_id).await;
        }

        // 7b. Clean up Sentry terminal issues (reuse the tracker from step 3b)
        if let Some(ref mut sentry) = sentry_tracker {
            cleanup_terminal_sentry(sentry, state, &workflow.config_rx, wf_id).await;
        }
    }

    // 8. Check open PRs for review feedback (always runs — has its own capacity guards)
    if config.pr_review.enabled {
        pr_review::check_and_dispatch_pr_reviews(
            state,
            &config,
            &workflow.config_rx,
            event_tx,
            wf_id,
            global_max_agents,
        )
        .await;
    }

    Ok(())
}

/// Spawn a worker task for a new issue.
fn dispatch_issue(
    issue: Issue,
    state: &OrchestratorState,
    config: &ServiceConfig,
    config_rx: &watch::Receiver<ServiceConfig>,
    event_tx: &mpsc::Sender<OrchestratorEvent>,
    workflow_id: &WorkflowId,
    base_branch: &str,
) {
    let state_key = workflow_id.state_key(&issue.identifier);
    tracing::info!(state_key, workflow = %workflow_id, base_branch, "dispatching worker");

    let stall_timeout = config.codex.stall_timeout();
    state.start_session(&state_key, issue.clone(), stall_timeout, &workflow_id.0);

    let config = config.clone();
    let config_rx = config_rx.clone();
    let event_tx = event_tx.clone();
    let wf_id = workflow_id.clone();
    let state_key_clone = state_key.clone();
    let base_branch = base_branch.to_string();

    let state_clone = state.clone();
    tokio::spawn(async move {
        let result = run_worker(
            &issue,
            &config,
            &config_rx,
            None,
            &state_clone,
            &state_key_clone,
            Some(&base_branch),
        )
        .await;

        let (success, error) = match &result {
            Ok(true) => (true, None),
            Ok(false) => (false, Some("max turns reached".to_string())),
            Err(e) => (false, Some(e.to_string())),
        };

        if let Err(e) = event_tx
            .send(OrchestratorEvent::WorkerCompleted {
                state_key: state_key_clone.clone(),
                success,
                error: error.clone(),
            })
            .await
        {
            tracing::error!(state_key = %state_key_clone, "failed to send WorkerCompleted event: {e}");
        }

        // Schedule retry on failure
        if !success {
            let base = Duration::from_secs(1);
            let max = Duration::from_secs(300);
            let delay = super::retry::calculate_backoff(0, base, max);
            state_clone.schedule_retry(&state_key_clone, 1, &wf_id.0);
            super::retry::schedule_retry(
                state_key_clone,
                1,
                delay,
                wf_id.0,
                event_tx,
            );
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
    workflow_id: &WorkflowId,
) {
    let state_key = retry.issue_id.clone();

    // Guard: don't dispatch a retry if the issue is still running (e.g. stale retry entry)
    if state.is_running(&state_key) {
        tracing::warn!(state_key, "skipping retry — session still running");
        return;
    }

    tracing::info!(state_key, workflow = %workflow_id, attempt = retry.attempt, "dispatching retry");

    let config = config.clone();
    let config_rx = config_rx.clone();
    let event_tx = event_tx.clone();
    let state_clone = state.clone();
    let attempt = retry.attempt;
    let wf_id = workflow_id.clone();
    let stall_timeout = config.codex.stall_timeout();

    tokio::spawn(async move {
        // Extract the issue_id from the state_key (strip workflow prefix)
        let issue_id = state_key
            .split_once('/')
            .map(|(_, id)| id)
            .unwrap_or(&state_key);

        let Some(mut issue) = fetch_issue_for_retry(issue_id, &config).await else {
            tracing::error!(
                state_key,
                attempt,
                "retry abandoned — issue could not be fetched from tracker"
            );
            return;
        };

        // Tag the re-fetched issue with the current workflow ID so that
        // downstream consumers (e.g. track_created_pr) see the correct workflow.
        issue.workflow_id = wf_id.0.clone();

        // Register the retry session
        state_clone.start_session(&state_key, issue.clone(), stall_timeout, &wf_id.0);

        let result = run_worker(
            &issue,
            &config,
            &config_rx,
            Some(attempt),
            &state_clone,
            &state_key,
            None, // retries use existing workspace, no base_branch override
        )
        .await;

        let (success, error) = match &result {
            Ok(true) => (true, None),
            Ok(false) => (false, Some("max turns reached".to_string())),
            Err(e) => (false, Some(e.to_string())),
        };

        if let Err(e) = event_tx
            .send(OrchestratorEvent::WorkerCompleted {
                state_key: state_key.clone(),
                success,
                error: error.clone(),
            })
            .await
        {
            tracing::error!(state_key, "failed to send WorkerCompleted event: {e}");
        }

        // Schedule further retry on failure, with increasing backoff
        if !success {
            let base = Duration::from_secs(1);
            let max = Duration::from_secs(300);
            let delay = super::retry::calculate_backoff(attempt, base, max);
            state_clone.schedule_retry(&state_key, attempt + 1, &wf_id.0);
            super::retry::schedule_retry(
                state_key,
                attempt + 1,
                delay,
                wf_id.0,
                event_tx,
            );
        }
    });
}

/// Re-fetch a single issue from the correct tracker for retry dispatch.
/// Returns `None` and logs errors if the issue cannot be fetched.
async fn fetch_issue_for_retry(issue_id: &str, config: &ServiceConfig) -> Option<Issue> {
    let is_sentry = config.sentry.enabled
        && !config.sentry.id_prefix.is_empty()
        && issue_id.starts_with(&config.sentry.id_prefix);
    if is_sentry {
        let mut tracker = match SentryTracker::new(config.sentry.clone()).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(issue_id, "retry: failed to create Sentry tracker: {e}");
                return None;
            }
        };
        match tracker
            .fetch_issue_states_by_ids(std::slice::from_ref(&issue_id.to_string()))
            .await
        {
            Ok(issues) => {
                let issue = issues.into_iter().next();
                if issue.is_none() {
                    tracing::warn!(issue_id, "retry: issue not found in Sentry");
                }
                issue
            }
            Err(e) => {
                tracing::error!(issue_id, "retry: failed to fetch Sentry issue: {e}");
                None
            }
        }
    } else {
        let mut tracker = match NotionTracker::new(config.tracker.clone()).await {
            Ok(t) => t,
            Err(e) => {
                tracing::error!(issue_id, "retry: failed to connect to tracker: {e}");
                return None;
            }
        };
        match tracker
            .fetch_issue_states_by_ids(std::slice::from_ref(&issue_id.to_string()))
            .await
        {
            Ok(issues) => {
                let issue = issues.into_iter().next();
                if issue.is_none() {
                    tracing::warn!(issue_id, "retry: issue not found in tracker");
                }
                issue
            }
            Err(e) => {
                tracing::error!(issue_id, "retry: failed to fetch issue: {e}");
                None
            }
        }
    }
}

/// Run a worker: prepare workspace, build prompt, start agent, run turns.
async fn run_worker(
    issue: &Issue,
    config: &ServiceConfig,
    config_rx: &watch::Receiver<ServiceConfig>,
    attempt: Option<u32>,
    state: &OrchestratorState,
    state_key: &str,
    base_branch: Option<&str>,
) -> Result<bool> {
    use crate::domain::session::{AgentEvent, AgentEventKind, RunStatus};
    use crate::workspace::hooks;

    let ws = WorkspaceManager::new(config_rx.clone());

    // Fetch comments from Notion before building the prompt
    let mut issue = issue.clone();
    if issue.source == "notion"
        && let Some(ref page_id) = issue.notion_page_id
    {
        match NotionTracker::new(config.tracker.clone()).await {
            Ok(mut tracker) => match tracker.fetch_comments(page_id).await {
                Ok(comments) => {
                    tracing::info!(
                        issue_id = issue.identifier,
                        count = comments.len(),
                        "fetched issue comments"
                    );
                    issue.comments = comments;
                }
                Err(e) => {
                    tracing::warn!(
                        issue_id = issue.identifier,
                        "failed to fetch comments: {e}"
                    );
                }
            },
            Err(e) => {
                tracing::warn!(
                    issue_id = issue.identifier,
                    "failed to connect to tracker for comments: {e}"
                );
            }
        }
    }

    // Ensure workspace exists (creates + runs after_create hook if new)
    state.push_agent_event(
        state_key,
        AgentEvent::now(AgentEventKind::Status {
            status: "Setting up workspace".into(),
        }),
    );
    let workspace_dir = ws.ensure(&issue, base_branch).await?;

    // Run before_run hook
    ws.prepare(&issue, attempt, base_branch).await?;

    // Run pre-flight verification (if enabled)
    if config.preflight.enabled {
        if config.preflight.prompt_template.is_empty() {
            tracing::warn!(
                issue_id = issue.identifier,
                "preflight is enabled but prompt_template is empty — skipping preflight"
            );
        } else if let Ok(mut preflight_prompt) = prompt::build_prompt(
            &config.preflight.prompt_template,
            &issue,
            attempt,
        ) {
            state.push_agent_event(
                state_key,
                AgentEvent::now(AgentEventKind::Status {
                    status: "Running preflight check".into(),
                }),
            );

            preflight_prompt.push_str(preflight_signal_instructions());

            // Use agent_subdirectory if configured (Liquid-rendered per issue)
            let preflight_dir = prompt::resolve_agent_dir(
                &workspace_dir,
                config.workspace.agent_subdirectory.as_deref(),
                &issue,
            );

            let preflight_runner = agent::AgentRunner::new(config.clone());
            match preflight_runner
                .start_session(&preflight_dir, &preflight_prompt, &issue.identifier)
                .await
            {
                Ok((mut preflight_worker, _preflight_mcp_guard)) => {
                    match run_agent_attempt(
                        &mut preflight_worker,
                        &preflight_prompt,
                        state,
                        state_key,
                    )
                    .await
                    {
                        Ok(_) => {}
                        Err(e) => {
                            tracing::warn!(
                                issue_id = issue.identifier,
                                "preflight agent failed: {e} — proceeding to main agent"
                            );
                            state.push_agent_event(
                                state_key,
                                AgentEvent::now(AgentEventKind::Error {
                                    message: format!("Preflight agent failed: {e}"),
                                }),
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        issue_id = issue.identifier,
                        "failed to start preflight agent: {e} — proceeding to main agent"
                    );
                    state.push_agent_event(
                        state_key,
                        AgentEvent::now(AgentEventKind::Error {
                            message: format!("Failed to start preflight agent: {e}"),
                        }),
                    );
                }
            }

            // Check if the preflight agent signaled to skip this issue
            let skip_path = preflight_dir.join("PREFLIGHT_SKIP");
            if tokio::fs::try_exists(&skip_path).await.unwrap_or(false) {
                let reason = tokio::fs::read_to_string(&skip_path)
                    .await
                    .unwrap_or_default();
                let reason = reason.trim();
                tracing::info!(
                    issue_id = issue.identifier,
                    reason,
                    "preflight: skipping issue"
                );
                if let Err(e) = tokio::fs::remove_file(&skip_path).await {
                    tracing::warn!(
                        issue_id = issue.identifier,
                        "failed to remove PREFLIGHT_SKIP file: {e}"
                    );
                }

                state.push_agent_event(
                    state_key,
                    AgentEvent::now(AgentEventKind::Status {
                        status: format!(
                            "Preflight: skipped — {}",
                            if reason.is_empty() {
                                "no reason given"
                            } else {
                                reason
                            }
                        ),
                    }),
                );

                ws.finish(&issue, true, base_branch).await?;
                return Ok(true);
            }
        } else if let Err(e) =
            prompt::build_prompt(&config.preflight.prompt_template, &issue, attempt)
        {
            tracing::warn!(
                issue_id = issue.identifier,
                error = %e,
                "failed to render preflight prompt template — skipping preflight"
            );
        }
    }

    // Build prompt from template, with PR metadata instructions appended.
    // The implementer has the best context for writing the initial PR description
    // since it performed the investigation and chose the fix.
    let mut prompt_text = prompt::build_prompt_full(
        &config.prompt_template,
        &issue,
        attempt,
        None,
        base_branch,
    )?;
    prompt_text.push_str(&pr_metadata_instructions(&issue));

    // Start agent session
    state.push_agent_event(
        state_key,
        AgentEvent::now(AgentEventKind::Status {
            status: "Starting agent".into(),
        }),
    );
    state.update_session_status(state_key, RunStatus::Running);

    // Use agent_subdirectory if configured (Liquid-rendered per issue)
    let agent_dir = prompt::resolve_agent_dir(
        &workspace_dir,
        config.workspace.agent_subdirectory.as_deref(),
        &issue,
    );

    let runner = agent::AgentRunner::new(config.clone());
    let (mut worker, _mcp_guard) = runner
        .start_session(&agent_dir, &prompt_text, &issue.identifier)
        .await?;

    // Run multi-turn agent loop
    let success =
        run_agent_attempt(&mut worker, &prompt_text, state, state_key).await?;

    if success {
        // Post-completion pipeline: commit → review → PR
        let hook_timeout = config.hooks.timeout();

        // 1. Run review agent (if enabled)
        if config.review.enabled {
            state.push_agent_event(
                state_key,
                AgentEvent::now(AgentEventKind::Status {
                    status: "Running deep review".into(),
                }),
            );

            // Run before_review hook if configured
            if let Some(ref hook_script) = config.review.before_review {
                let rendered = prompt::build_prompt_with_workspace(
                    hook_script,
                    &issue,
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

            let mut review_prompt = build_review_prompt(&issue, &config.review.prompt_template);
            // Ask the reviewer to update the PR metadata the implementer wrote,
            // accounting for any changes the review introduced.
            review_prompt.push_str(pr_metadata_update_instructions());
            match runner
                .start_session(&agent_dir, &review_prompt, &issue.identifier)
                .await
            {
                Ok((mut review_worker, _review_mcp_guard)) => {
                    match run_agent_attempt(
                        &mut review_worker,
                        &review_prompt,
                        state,
                        state_key,
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
            state_key,
            AgentEvent::now(AgentEventKind::Status {
                status: "Opening draft PR".into(),
            }),
        );

        // Read agent-generated PR metadata, falling back to defaults
        let (pr_title, pr_body_text) =
            read_pr_metadata(&workspace_dir, &issue).await;

        // Write title/body to temp files outside the workspace to avoid accidental git add
        let tmp = std::env::temp_dir();
        let title_file = tmp.join(format!("symposium-pr-title-{}", issue.identifier));
        let body_file = tmp.join(format!("symposium-pr-body-{}", issue.identifier));
        if let Err(e) = tokio::fs::write(&title_file, &pr_title).await {
            tracing::warn!(path = %title_file.display(), "failed to write PR title temp file: {e}");
        }
        if let Err(e) = tokio::fs::write(&body_file, &pr_body_text).await {
            tracing::warn!(path = %body_file.display(), "failed to write PR body temp file: {e}");
        }
        let default_branch = &config.workspace.default_branch;
        let base_flag = match base_branch {
            Some(b) if b != default_branch => format!(" --base {b}"),
            _ => String::new(),
        };
        let pr_script = format!(
            "git push -u origin HEAD 2>&1 && gh pr create --draft{base_flag} --title \"$(cat {})\" --body-file {} 2>&1",
            title_file.display(),
            body_file.display(),
        );
        match hooks::run_hook(&pr_script, &workspace_dir, hook_timeout).await {
            Ok(()) => {
                tracing::info!(issue_id = issue.identifier, "draft PR created");
                state.push_agent_event(
                    state_key,
                    AgentEvent::now(AgentEventKind::Status {
                        status: "Draft PR opened".into(),
                    }),
                );

                // Track PR for review monitoring
                if config.pr_review.enabled {
                    track_created_pr(
                        &issue,
                        &workspace_dir,
                        state,
                        state_key,
                        &issue.workflow_id,
                        hook_timeout,
                    )
                    .await;
                }

                // Update Notion status on PR creation (e.g. set to "In Review")
                if let Some(ref target_status) = config.tracker.on_pr_created_status
                    && let Some(ref page_id) = issue.notion_page_id
                {
                    update_notion_status(
                        &config.tracker,
                        page_id,
                        &config.tracker.property_status,
                        target_status,
                    )
                    .await;
                }
            }
            Err(e) => {
                tracing::warn!(issue_id = issue.identifier, "PR creation failed: {e}");
                state.push_agent_event(
                    state_key,
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
    ws.finish(&issue, success, base_branch).await?;

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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default_pr_title(issue),
        Err(e) => {
            tracing::warn!(path = %title_path.display(), "failed to read PR_TITLE: {e}");
            default_pr_title(issue)
        }
    };

    let pr_body = match tokio::fs::read_to_string(&body_path).await {
        Ok(contents) => {
            let _ = tokio::fs::remove_file(&body_path).await;
            let body = contents.trim().to_string();
            if body.is_empty() { default_pr_body(issue) } else { body }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => default_pr_body(issue),
        Err(e) => {
            tracing::warn!(path = %body_path.display(), "failed to read PR_BODY.md: {e}");
            default_pr_body(issue)
        }
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

/// Instructions appended to the preflight prompt for signaling skip/proceed.
fn preflight_signal_instructions() -> &'static str {
    r#"

After your investigation, decide whether this issue should proceed to the implementation phase:

- If the issue is NOT reproducible, already fixed, or not a real issue: write a file called `PREFLIGHT_SKIP` in your current working directory containing a brief explanation of why this issue should be skipped. Do NOT create any branches or make code changes.
- If the issue IS valid and should be fixed: do NOT create a `PREFLIGHT_SKIP` file. Simply finish — the system will proceed to the implementation phase automatically.

Do NOT commit any files. Do NOT create pull requests."#
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
fn pr_metadata_update_instructions() -> &'static str {
    r#"

Finally, read the `PR_TITLE` and `PR_BODY.md` files in the root of the git repository. These were written by the implementer. If you made any changes during your review, update these files to reflect the final state of the branch. If you made no changes, leave them as-is. Do NOT git-commit these files."#
}


/// Remove workspaces for issues that have reached terminal states.
///
/// Skips removal for issues that still have tracked open PRs (the PR review
/// monitor needs the workspace to exist for dispatching fix agents).
fn remove_terminal_workspaces(
    terminal_issues: Vec<Issue>,
    state: &OrchestratorState,
    config_rx: &watch::Receiver<ServiceConfig>,
    workflow_id: &WorkflowId,
) {
    let ws = WorkspaceManager::new(config_rx.clone());
    let tracked_pr_ids: std::collections::HashSet<String> = state
        .open_prs()
        .iter()
        .map(|pr| pr.issue.identifier.clone())
        .collect();
    let state = state.clone();
    let wf_id = workflow_id.clone();
    tokio::spawn(async move {
        for issue in terminal_issues {
            if tracked_pr_ids.contains(&issue.identifier) {
                tracing::debug!(
                    issue_id = issue.identifier,
                    "skipping workspace removal — PR still tracked"
                );
                continue;
            }
            let sk = wf_id.state_key(&issue.identifier);
            if !state.is_running(&sk)
                && let Err(e) = ws.remove(&issue.identifier).await
            {
                tracing::warn!(
                    issue_id = issue.identifier,
                    "failed to remove terminal workspace: {e}"
                );
            }
        }
    });
}

/// Clean up workspaces for issues that have reached terminal states.
async fn cleanup_terminal(
    tracker: &mut NotionTracker,
    state: &OrchestratorState,
    config_rx: &watch::Receiver<ServiceConfig>,
    workflow_id: &WorkflowId,
) {
    match tracker.fetch_terminal_issues().await {
        Ok(issues) => remove_terminal_workspaces(issues, state, config_rx, workflow_id),
        Err(e) => tracing::warn!(workflow = %workflow_id, "failed to fetch terminal issues for cleanup: {e}"),
    }
}

/// Extract the PR number and branch after creation and register for review monitoring.
async fn track_created_pr(
    issue: &Issue,
    workspace_dir: &std::path::Path,
    state: &OrchestratorState,
    state_key: &str,
    workflow_id: &str,
    timeout: Duration,
) {
    let script = "gh pr view --json number,headRefName";
    match workspace_hooks::run_hook_with_output(script, workspace_dir, timeout).await {
        Ok(output) => {
            match serde_json::from_str::<serde_json::Value>(output.trim()) {
                Ok(data) => {
                    if let Some(number) = data["number"].as_u64() {
                        let branch_name = data["headRefName"]
                            .as_str()
                            .unwrap_or("")
                            .to_string();
                        state.track_pr(
                            state_key,
                            issue.clone(),
                            number,
                            workspace_dir.to_path_buf(),
                            workflow_id,
                            &branch_name,
                        );
                        tracing::info!(
                            issue_id = issue.identifier,
                            pr = number,
                            branch = %branch_name,
                            "tracking PR for review monitoring"
                        );
                    } else {
                        tracing::warn!(
                            issue_id = issue.identifier,
                            output = output.trim(),
                            "gh pr view returned JSON without a valid 'number' field"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        issue_id = issue.identifier,
                        "failed to parse gh pr view output: {e}"
                    );
                }
            }
        }
        Err(e) => {
            tracing::warn!(
                issue_id = issue.identifier,
                "failed to get PR info for tracking: {e}"
            );
        }
    }
}

/// Update a Notion page's status via MCP. Best-effort; logs warnings on failure.
pub(super) async fn update_notion_status(
    tracker_config: &crate::config::schema::TrackerConfig,
    page_id: &str,
    status_property: &str,
    status_value: &str,
) {
    match NotionTracker::new(tracker_config.clone()).await {
        Ok(mut tracker) => {
            if let Err(e) = tracker
                .update_issue_status(page_id, status_property, status_value)
                .await
            {
                tracing::warn!(
                    page_id,
                    status = status_value,
                    "failed to update Notion status: {e}"
                );
            }
        }
        Err(e) => {
            tracing::warn!(
                page_id,
                "failed to connect to Notion for status update: {e}"
            );
        }
    }
}

/// Clean up workspaces for Sentry issues that have been resolved.
async fn cleanup_terminal_sentry(
    tracker: &mut SentryTracker,
    state: &OrchestratorState,
    config_rx: &watch::Receiver<ServiceConfig>,
    workflow_id: &WorkflowId,
) {
    match tracker.fetch_terminal_issues().await {
        Ok(issues) => remove_terminal_workspaces(issues, state, config_rx, workflow_id),
        Err(e) => tracing::warn!(workflow = %workflow_id, "failed to fetch terminal Sentry issues for cleanup: {e}"),
    }
}
