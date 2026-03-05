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

    tokio::spawn(async move {
        let result = run_worker(&issue, &config, &config_rx, None).await;

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
    _state: &OrchestratorState,
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

        let result = run_worker(&issue, &config, &config_rx, Some(attempt)).await;

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
) -> Result<bool> {
    let ws = WorkspaceManager::new(config_rx.clone());

    // Ensure workspace exists (creates + runs after_create hook if new)
    let workspace_dir = ws.ensure(&issue.identifier).await?;

    // Run before_run hook
    ws.prepare(&issue.identifier).await?;

    // Build prompt from template
    let prompt_text = prompt::build_prompt(&config.prompt_template, issue, attempt)?;

    // Start agent session
    let runner = agent::AgentRunner::new(config.clone());
    let mut worker = runner
        .start_session(&workspace_dir, &prompt_text, &issue.identifier)
        .await?;

    // Run multi-turn agent loop
    let success = run_agent_attempt(&mut worker, &prompt_text, config.agent.max_turns).await?;

    // Run after_run hook
    ws.finish(&issue.identifier, success).await?;

    Ok(success)
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
