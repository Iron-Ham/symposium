use crate::agent;
use crate::agent::worker::run_agent_attempt;
use crate::config::schema::{ReviewerFilter, ServiceConfig};
use crate::domain::issue::Issue;
use crate::domain::session::{AgentEvent, AgentEventKind, RunStatus};
use crate::domain::state::{OpenPr, OrchestratorState};
use crate::domain::workflow::WorkflowId;
use crate::error::Result;
use crate::prompt;
use crate::workspace::hooks;
use crate::workspace::WorkspaceManager;

use super::OrchestratorEvent;
use chrono::{DateTime, Utc};
use serde_json::Value;
use std::path::Path;
use std::time::Duration;
use tokio::sync::{mpsc, watch};

const GH_TIMEOUT: Duration = Duration::from_secs(30);

/// PR status retrieved from `gh pr view`.
struct PrStatus {
    state: String,
    latest_actionable_review_at: Option<DateTime<Utc>>,
}

/// Check all tracked open PRs and dispatch review workers as needed.
pub async fn check_and_dispatch_pr_reviews(
    state: &OrchestratorState,
    config: &ServiceConfig,
    config_rx: &watch::Receiver<ServiceConfig>,
    event_tx: &mpsc::Sender<OrchestratorEvent>,
    workflow_id: &WorkflowId,
    global_max_agents: Option<usize>,
) {
    let open_prs = state.open_prs();
    // Only process PRs belonging to this workflow
    let workflow_prs: Vec<_> = open_prs
        .into_iter()
        .filter(|pr| pr.workflow_id == workflow_id.0)
        .collect();
    if workflow_prs.is_empty() {
        return;
    }

    tracing::debug!(
        workflow = %workflow_id,
        count = workflow_prs.len(),
        "checking open PRs for review feedback"
    );

    for pr in workflow_prs {
        if state.running_count_for_workflow(&workflow_id.0) >= config.agent.max_concurrent_agents {
            tracing::debug!("at per-workflow capacity, skipping remaining PR review checks");
            break;
        }
        if let Some(global_max) = global_max_agents
            && state.running_count() >= global_max
        {
            tracing::debug!("at global capacity, skipping remaining PR review checks");
            break;
        }

        let review_state_key = review_session_state_key(&pr.issue.identifier, workflow_id);
        if state.is_running(&review_state_key) {
            continue;
        }

        // The pr_state_key is the key under which the PR is tracked in open_prs
        let pr_state_key = workflow_id.state_key(&pr.issue.identifier);

        match check_pr_status(&pr.workspace_dir, pr.pr_number, &config.pr_review.reviewers).await {
            Ok(status) => {
                if is_terminal(&status) {
                    tracing::info!(
                        issue_id = pr.issue.identifier,
                        pr = pr.pr_number,
                        state = status.state,
                        "PR reached terminal state, untracking"
                    );
                    state.untrack_pr(&pr_state_key);
                    continue;
                }

                if needs_attention(&pr, &status) {
                    dispatch_pr_review(pr, state, config, config_rx, event_tx, workflow_id);
                }
            }
            Err(e) => {
                tracing::warn!(
                    issue_id = pr.issue.identifier,
                    pr = pr.pr_number,
                    "failed to check PR status: {e}"
                );
            }
        }
    }
}

/// State key for a PR review worker session.
fn review_session_state_key(issue_id: &str, workflow_id: &WorkflowId) -> String {
    workflow_id.state_key(&format!("pr-review:{issue_id}"))
}

/// Query a PR's review status via `gh pr view`.
async fn check_pr_status(
    workspace_dir: &Path,
    pr_number: u64,
    reviewer_filter: &ReviewerFilter,
) -> Result<PrStatus> {
    let script = format!("gh pr view {pr_number} --json state,reviewDecision,reviews");
    let output = hooks::run_hook_with_output(&script, workspace_dir, GH_TIMEOUT).await?;
    let data: Value = serde_json::from_str(output.trim())?;

    let state = data["state"].as_str().unwrap_or("").to_string();

    // Group reviews by author, keeping only each author's latest review.
    // This handles the case where a reviewer leaves comments and then later approves —
    // we only care about their most recent review state.
    let latest_actionable_review_at = latest_actionable_review_at(
        data["reviews"].as_array(),
        reviewer_filter,
    );

    Ok(PrStatus {
        state,
        latest_actionable_review_at,
    })
}

/// Find the latest actionable review timestamp, considering only each author's
/// most recent review. A review is actionable if it's `CHANGES_REQUESTED` or
/// `COMMENTED` — most reviewers leave inline comments without formally requesting
/// changes. If a reviewer's latest review is `APPROVED` or `DISMISSED`, their
/// earlier feedback is considered addressed and ignored.
fn latest_actionable_review_at(
    reviews: Option<&Vec<Value>>,
    reviewer_filter: &ReviewerFilter,
) -> Option<DateTime<Utc>> {
    use std::collections::HashMap;

    let reviews = reviews?;

    // Collect each author's latest review (by submittedAt)
    let mut latest_per_author: HashMap<&str, &Value> = HashMap::new();
    for review in reviews {
        let login = review["author"]["login"].as_str().unwrap_or("");
        let submitted = review["submittedAt"].as_str().unwrap_or("");
        let existing_submitted = latest_per_author
            .get(login)
            .and_then(|r| r["submittedAt"].as_str())
            .unwrap_or("");
        if submitted > existing_submitted {
            latest_per_author.insert(login, review);
        }
    }

    // From each author's latest review, pick those that contain feedback
    // (CHANGES_REQUESTED or COMMENTED). APPROVED/DISMISSED reviews mean
    // the author is satisfied and their earlier comments are addressed.
    latest_per_author
        .values()
        .filter(|r| {
            matches!(
                r["state"].as_str(),
                Some("CHANGES_REQUESTED" | "COMMENTED")
            )
        })
        .filter(|r| is_reviewer_actionable(r, reviewer_filter))
        .filter_map(|r| r["submittedAt"].as_str())
        .filter_map(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|dt| dt.with_timezone(&Utc))
        .max()
}

/// Check whether a review matches the configured reviewer filter.
fn is_reviewer_actionable(review: &Value, filter: &ReviewerFilter) -> bool {
    match filter {
        ReviewerFilter::All => true,
        ReviewerFilter::Humans => {
            let login = review["author"]["login"].as_str().unwrap_or("");
            !login.ends_with("[bot]")
        }
        ReviewerFilter::Specific(usernames) => {
            let login = review["author"]["login"].as_str().unwrap_or("");
            usernames.iter().any(|u| u.eq_ignore_ascii_case(login))
        }
    }
}

/// Does this PR have new review feedback we haven't yet addressed?
fn needs_attention(pr: &OpenPr, status: &PrStatus) -> bool {
    match (status.latest_actionable_review_at, pr.last_addressed_at) {
        (Some(requested), Some(addressed)) => requested > addressed,
        (Some(_), None) => true,
        _ => false,
    }
}

/// Is the PR merged or closed?
fn is_terminal(status: &PrStatus) -> bool {
    status.state == "MERGED" || status.state == "CLOSED"
}

/// Spawn a worker task to address PR review feedback.
fn dispatch_pr_review(
    pr: OpenPr,
    state: &OrchestratorState,
    config: &ServiceConfig,
    config_rx: &watch::Receiver<ServiceConfig>,
    event_tx: &mpsc::Sender<OrchestratorEvent>,
    workflow_id: &WorkflowId,
) {
    let review_state_key = review_session_state_key(&pr.issue.identifier, workflow_id);
    let pr_state_key = workflow_id.state_key(&pr.issue.identifier);
    tracing::info!(
        issue_id = pr.issue.identifier,
        pr = pr.pr_number,
        review_state_key,
        "dispatching PR review worker"
    );

    let stall_timeout = config.codex.stall_timeout();
    state.start_session(
        &review_state_key,
        Issue {
            identifier: format!("pr-review:{}", pr.issue.identifier),
            title: format!("PR review: {}", pr.issue.title),
            source: "pr_review".to_string(),
            workflow_id: workflow_id.0.clone(),
            ..pr.issue.clone()
        },
        stall_timeout,
        &workflow_id.0,
    );

    let config = config.clone();
    let config_rx = config_rx.clone();
    let event_tx = event_tx.clone();
    let state_clone = state.clone();

    tokio::spawn(async move {
        let result = run_pr_review_worker(
            &pr,
            &review_state_key,
            &config,
            &config_rx,
            &state_clone,
        )
        .await;

        let (success, error) = match &result {
            Ok(true) => (true, None),
            Ok(false) => (false, Some("max turns reached".to_string())),
            Err(e) => (false, Some(e.to_string())),
        };

        if success {
            state_clone.mark_pr_addressed(&pr_state_key);
        }

        if let Err(e) = event_tx
            .send(OrchestratorEvent::WorkerCompleted {
                state_key: review_state_key.clone(),
                success,
                error,
            })
            .await
        {
            tracing::error!(state_key = %review_state_key, "failed to send WorkerCompleted event: {e}");
        }

        // No retry scheduling here — unlike issue workers, the PR review polling loop
        // will re-detect unaddressed reviews on the next tick and dispatch again.
    });
}

/// Run the PR review agent: prepare workspace, build prompt, start agent, push fixes.
async fn run_pr_review_worker(
    pr: &OpenPr,
    review_state_key: &str,
    config: &ServiceConfig,
    config_rx: &watch::Receiver<ServiceConfig>,
    state: &OrchestratorState,
) -> Result<bool> {
    // Use the stored workspace path from PR creation time rather than recomputing
    // from the issue ID (which could differ if workspace root config was hot-reloaded).
    let workspace_dir = &pr.workspace_dir;
    if !workspace_dir.exists() {
        return Err(crate::error::Error::Workspace(format!(
            "workspace for {} no longer exists at {}",
            pr.issue.identifier,
            workspace_dir.display(),
        )));
    }

    // Run before_run hook (e.g. git fetch, git rebase)
    state.push_agent_event(
        review_state_key,
        AgentEvent::now(AgentEventKind::Status {
            status: "Preparing workspace for PR review".into(),
        }),
    );
    let ws = WorkspaceManager::new(config_rx.clone());
    ws.prepare(&pr.issue, None).await?;

    // Build prompt
    let prompt_text = build_pr_review_prompt(
        &pr.issue,
        pr.pr_number,
        &config.pr_review.prompt_template,
    );

    // Start agent
    state.push_agent_event(
        review_state_key,
        AgentEvent::now(AgentEventKind::Status {
            status: "Starting PR review agent".into(),
        }),
    );
    state.update_session_status(review_state_key, RunStatus::Running);

    let agent_dir = match &config.workspace.agent_subdirectory {
        Some(sub) => workspace_dir.join(sub),
        None => workspace_dir.clone(),
    };

    let runner = agent::AgentRunner::new(config.clone());
    let (mut worker, _mcp_guard) = runner
        .start_session(&agent_dir, &prompt_text, &pr.issue.identifier)
        .await?;

    let success = run_agent_attempt(&mut worker, &prompt_text, state, review_state_key).await?;

    // Run after_run hook
    ws.finish(&pr.issue, success).await?;

    Ok(success)
}

/// Build a prompt for addressing PR review feedback.
fn build_pr_review_prompt(issue: &Issue, pr_number: u64, custom_template: &str) -> String {
    if !custom_template.is_empty() {
        let mut issue_for_template = issue.clone();
        issue_for_template
            .extra
            .insert("pr_number".to_string(), pr_number.to_string());
        return prompt::build_prompt(custom_template, &issue_for_template, None).unwrap_or_else(
            |e| {
                tracing::warn!("failed to render custom PR review template: {e}, using default");
                build_default_pr_review_prompt(issue, pr_number)
            },
        );
    }
    build_default_pr_review_prompt(issue, pr_number)
}

fn build_default_pr_review_prompt(issue: &Issue, pr_number: u64) -> String {
    format!(
        r#"PR #{pr_number} for issue {id}: "{title}" has changes requested by reviewers.

First, read `CLAUDE.md` at the repo root and any relevant subsystem docs.

Then review the feedback:

1. Run `gh pr view {pr_number} --comments` to see all review comments and discussion
2. Run `gh pr diff {pr_number}` to see the current changes on this PR
3. Run `gh pr checks {pr_number}` to check CI status

Address each piece of reviewer feedback:

- For code change requests: make the requested modifications
- For questions: if they reveal a real issue, fix it; otherwise leave a PR comment explaining
- For nits/style suggestions: fix them
- For suggestions you disagree with: leave a PR comment explaining your reasoning

After making all changes:

1. Stage and commit your changes with a descriptive message
2. Push to update the PR: `git push`

Do NOT create a new PR — the existing PR #{pr_number} will be updated automatically when you push."#,
        id = issue.identifier,
        title = issue.title,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn make_review(state: &str, login: &str, submitted_at: &str) -> Value {
        serde_json::json!({
            "state": state,
            "author": { "login": login },
            "submittedAt": submitted_at,
        })
    }

    #[test]
    fn is_reviewer_actionable_all() {
        let review = make_review("CHANGES_REQUESTED", "some-bot[bot]", "2026-01-01T00:00:00Z");
        assert!(is_reviewer_actionable(&review, &ReviewerFilter::All));
    }

    #[test]
    fn is_reviewer_actionable_humans_filters_bots() {
        let bot = make_review("CHANGES_REQUESTED", "dependabot[bot]", "2026-01-01T00:00:00Z");
        let human = make_review("CHANGES_REQUESTED", "alice", "2026-01-01T00:00:00Z");
        assert!(!is_reviewer_actionable(&bot, &ReviewerFilter::Humans));
        assert!(is_reviewer_actionable(&human, &ReviewerFilter::Humans));
    }

    #[test]
    fn is_reviewer_actionable_specific_users() {
        let filter = ReviewerFilter::Specific(vec!["alice".to_string(), "Bob".to_string()]);
        let alice = make_review("CHANGES_REQUESTED", "Alice", "2026-01-01T00:00:00Z");
        let bob = make_review("CHANGES_REQUESTED", "bob", "2026-01-01T00:00:00Z");
        let carol = make_review("CHANGES_REQUESTED", "carol", "2026-01-01T00:00:00Z");
        assert!(is_reviewer_actionable(&alice, &filter));
        assert!(is_reviewer_actionable(&bob, &filter));
        assert!(!is_reviewer_actionable(&carol, &filter));
    }

    #[test]
    fn needs_attention_no_reviews() {
        let pr = OpenPr {
            issue: test_issue(),
            pr_number: 1,
            workspace_dir: "/tmp".into(),
            last_addressed_at: None,
            workflow_id: "default".to_string(),
        };
        let status = PrStatus {
            state: "OPEN".into(),
            latest_actionable_review_at: None,
        };
        assert!(!needs_attention(&pr, &status));
    }

    #[test]
    fn needs_attention_new_review() {
        let pr = OpenPr {
            issue: test_issue(),
            pr_number: 1,
            workspace_dir: "/tmp".into(),
            last_addressed_at: None,
            workflow_id: "default".to_string(),
        };
        let status = PrStatus {
            state: "OPEN".into(),
            latest_actionable_review_at: Some(Utc::now()),
        };
        assert!(needs_attention(&pr, &status));
    }

    #[test]
    fn needs_attention_already_addressed() {
        let now = Utc::now();
        let pr = OpenPr {
            issue: test_issue(),
            pr_number: 1,
            workspace_dir: "/tmp".into(),
            last_addressed_at: Some(now),
            workflow_id: "default".to_string(),
        };
        let status = PrStatus {
            state: "OPEN".into(),
            latest_actionable_review_at: Some(now - chrono::Duration::hours(1)),
        };
        assert!(!needs_attention(&pr, &status));
    }

    #[test]
    fn needs_attention_new_review_after_addressed() {
        let now = Utc::now();
        let pr = OpenPr {
            issue: test_issue(),
            pr_number: 1,
            workspace_dir: "/tmp".into(),
            last_addressed_at: Some(now - chrono::Duration::hours(1)),
            workflow_id: "default".to_string(),
        };
        let status = PrStatus {
            state: "OPEN".into(),
            latest_actionable_review_at: Some(now),
        };
        assert!(needs_attention(&pr, &status));
    }

    #[test]
    fn terminal_states() {
        assert!(is_terminal(&PrStatus {
            state: "MERGED".into(),
            latest_actionable_review_at: None,
        }));
        assert!(is_terminal(&PrStatus {
            state: "CLOSED".into(),
            latest_actionable_review_at: None,
        }));
        assert!(!is_terminal(&PrStatus {
            state: "OPEN".into(),
            latest_actionable_review_at: None,
        }));
    }

    #[test]
    fn default_prompt_includes_pr_number() {
        let issue = test_issue();
        let prompt = build_default_pr_review_prompt(&issue, 42);
        assert!(prompt.contains("PR #42"));
        assert!(prompt.contains("TASK-123"));
        assert!(prompt.contains("Fix the bug"));
    }

    #[test]
    fn superseded_review_is_ignored() {
        // Reviewer requests changes, then approves — should NOT trigger
        let reviews = vec![
            make_review("CHANGES_REQUESTED", "alice", "2026-01-01T10:00:00Z"),
            make_review("APPROVED", "alice", "2026-01-02T10:00:00Z"),
        ];
        let result = latest_actionable_review_at(Some(&reviews), &ReviewerFilter::All);
        assert!(result.is_none());
    }

    #[test]
    fn comment_then_approve_is_ignored() {
        // Reviewer leaves comments, then approves — should NOT trigger
        let reviews = vec![
            make_review("COMMENTED", "alice", "2026-01-01T10:00:00Z"),
            make_review("APPROVED", "alice", "2026-01-02T10:00:00Z"),
        ];
        let result = latest_actionable_review_at(Some(&reviews), &ReviewerFilter::All);
        assert!(result.is_none());
    }

    #[test]
    fn commented_review_triggers() {
        // A COMMENTED review (most common kind) should trigger
        let reviews = vec![
            make_review("COMMENTED", "alice", "2026-01-01T10:00:00Z"),
        ];
        let result = latest_actionable_review_at(Some(&reviews), &ReviewerFilter::All);
        assert!(result.is_some());
    }

    #[test]
    fn superseded_review_different_authors() {
        // Alice approves after commenting, but Bob still has unaddressed comments
        let reviews = vec![
            make_review("COMMENTED", "alice", "2026-01-01T10:00:00Z"),
            make_review("APPROVED", "alice", "2026-01-02T10:00:00Z"),
            make_review("COMMENTED", "bob", "2026-01-02T12:00:00Z"),
        ];
        let result = latest_actionable_review_at(Some(&reviews), &ReviewerFilter::All);
        assert!(result.is_some());
    }

    #[test]
    fn no_reviews_returns_none() {
        let reviews = vec![];
        let result = latest_actionable_review_at(Some(&reviews), &ReviewerFilter::All);
        assert!(result.is_none());
    }

    #[test]
    fn only_approved_reviews_returns_none() {
        let reviews = vec![
            make_review("APPROVED", "alice", "2026-01-01T10:00:00Z"),
            make_review("APPROVED", "bob", "2026-01-02T10:00:00Z"),
        ];
        let result = latest_actionable_review_at(Some(&reviews), &ReviewerFilter::All);
        assert!(result.is_none());
    }

    #[test]
    fn real_world_mixed_reviews() {
        // Simulates typical PR data: bot comments, human comments, human approves
        let reviews = vec![
            make_review("COMMENTED", "ci-bot", "2026-03-06T21:45:13Z"),
            make_review("COMMENTED", "bot-assistant", "2026-03-06T22:14:25Z"),
            make_review("APPROVED", "reviewer-a", "2026-03-10T15:07:10Z"),
            make_review("COMMENTED", "reviewer-b", "2026-03-10T17:21:15Z"),
        ];
        // With "all" filter: ci-bot, bot-assistant, and reviewer-b all have COMMENTED as latest
        let result = latest_actionable_review_at(Some(&reviews), &ReviewerFilter::All);
        assert!(result.is_some());

        // With "humans" filter: ci-bot is not filtered (doesn't end in [bot])
        // but bot-assistant and reviewer-b still trigger
        let result = latest_actionable_review_at(Some(&reviews), &ReviewerFilter::Humans);
        assert!(result.is_some());

        // With specific filter for only reviewer-a (who approved): no trigger
        let filter = ReviewerFilter::Specific(vec!["reviewer-a".to_string()]);
        let result = latest_actionable_review_at(Some(&reviews), &filter);
        assert!(result.is_none());
    }

    /// Dry-run: feed representative `gh pr view` JSON through the full
    /// status parsing pipeline and verify the decision logic.
    #[test]
    fn dry_run_real_gh_output() {
        // Representative output matching: gh pr view 42 --repo acme-org/acme-app --json state,reviewDecision,reviews
        let raw = r#"{"reviewDecision":"REVIEW_REQUIRED","reviews":[{"id":"PRR_fake00000000001","author":{"login":"ci-bot"},"authorAssociation":"NONE","body":"review body","submittedAt":"2026-03-06T21:45:13Z","state":"COMMENTED","commit":{"oid":"aaa1111111"}},{"id":"PRR_fake00000000002","author":{"login":"bot-assistant"},"authorAssociation":"NONE","body":"","submittedAt":"2026-03-06T22:14:25Z","state":"COMMENTED","commit":{"oid":"bbb2222222"}}],"state":"OPEN"}"#;

        let data: Value = serde_json::from_str(raw).unwrap();
        let state = data["state"].as_str().unwrap_or("").to_string();

        // Verify state parsing
        assert_eq!(state, "OPEN");
        assert!(!is_terminal(&PrStatus {
            state: state.clone(),
            latest_actionable_review_at: None,
        }));

        // With "all" filter — both COMMENTED reviews are actionable
        let result = latest_actionable_review_at(
            data["reviews"].as_array(),
            &ReviewerFilter::All,
        );
        assert!(result.is_some());
        // Latest should be bot-assistant's review at 22:14:25Z
        let ts = result.unwrap();
        assert_eq!(
            ts,
            DateTime::parse_from_rfc3339("2026-03-06T22:14:25Z")
                .unwrap()
                .with_timezone(&Utc)
        );

        // With specific filter — only react to a reviewer not present
        let filter = ReviewerFilter::Specific(vec!["reviewer-a".to_string()]);
        let result = latest_actionable_review_at(data["reviews"].as_array(), &filter);
        assert!(result.is_none(), "no matching reviewer should mean no trigger");

        // Simulate an OpenPr that was never addressed — should need attention
        let pr = OpenPr {
            issue: test_issue(),
            pr_number: 42,
            workspace_dir: "/tmp/ws".into(),
            last_addressed_at: None,
            workflow_id: "default".to_string(),
        };
        let status = PrStatus {
            state,
            latest_actionable_review_at: Some(ts),
        };
        assert!(needs_attention(&pr, &status));

        // After addressing, same review should NOT need attention
        let pr_addressed = OpenPr {
            last_addressed_at: Some(Utc::now()),
            ..pr
        };
        assert!(!needs_attention(&pr_addressed, &status));
    }

    /// Dry-run: feed representative `gh pr view` JSON (APPROVED) through
    /// the pipeline and verify it does NOT trigger for the approver.
    #[test]
    fn dry_run_approved_pr() {
        // Has COMMENTED reviews from bot-assistant and reviewer-b, but APPROVED by reviewer-a
        let raw = r#"{"reviewDecision":"APPROVED","reviews":[{"author":{"login":"bot-assistant"},"state":"COMMENTED","submittedAt":"2026-03-06T15:00:38Z"},{"author":{"login":"reviewer-a"},"state":"APPROVED","submittedAt":"2026-03-10T15:07:10Z"},{"author":{"login":"reviewer-b"},"state":"COMMENTED","submittedAt":"2026-03-10T15:42:58Z"},{"author":{"login":"reviewer-b"},"state":"COMMENTED","submittedAt":"2026-03-10T17:21:15Z"}],"state":"OPEN"}"#;

        let data: Value = serde_json::from_str(raw).unwrap();

        // bot-assistant's latest review: COMMENTED (actionable)
        // reviewer-a's latest: APPROVED (not actionable)
        // reviewer-b's latest: COMMENTED at 17:21 (actionable)
        let result = latest_actionable_review_at(
            data["reviews"].as_array(),
            &ReviewerFilter::All,
        );
        assert!(result.is_some());

        // If we filter to only reviewer-a (the approver), no trigger
        let filter = ReviewerFilter::Specific(vec!["reviewer-a".to_string()]);
        let result = latest_actionable_review_at(data["reviews"].as_array(), &filter);
        assert!(result.is_none());
    }

    fn test_issue() -> Issue {
        Issue {
            identifier: "TASK-123".to_string(),
            title: "Fix the bug".to_string(),
            description: Some("Something is broken".to_string()),
            status: "In Progress".to_string(),
            priority: Some("High".to_string()),
            url: None,
            notion_page_id: None,
            blockers: vec![],
            source: "notion".to_string(),
            extra: HashMap::new(),
            comments: vec![],
            workflow_id: "default".to_string(),
        }
    }
}
