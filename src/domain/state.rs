use super::issue::Issue;
use super::retry::RetryEntry;
use super::session::LiveSession;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_COMPLETED: usize = 50;

#[derive(Debug, Clone)]
pub struct OrchestratorState {
    inner: Arc<Mutex<StateInner>>,
}

#[derive(Debug)]
struct StateInner {
    running: HashMap<String, RunningEntry>,
    retries: HashMap<String, RetryEntry>,
    completed: Vec<CompletedEntry>,
    tokens: TokenTotals,
    open_prs: HashMap<String, OpenPr>,
    state_dir: Option<PathBuf>,
}

/// A PR opened by Symposium that is being monitored for review feedback.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenPr {
    pub issue: Issue,
    pub pr_number: u64,
    pub workspace_dir: PathBuf,
    pub last_addressed_at: Option<DateTime<Utc>>,
    pub workflow_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunningEntry {
    pub issue: Issue,
    pub session: LiveSession,
    pub stall_timeout: Duration,
    pub workflow_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletedEntry {
    pub issue_id: String,
    pub success: bool,
    pub error: Option<String>,
    pub completed_at: DateTime<Utc>,
    pub attempts: u32,
    pub workflow_id: String,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TokenTotals {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct StateSnapshot {
    pub running: Vec<RunningEntry>,
    pub retries: Vec<String>,
    pub completed: Vec<CompletedEntry>,
    pub tokens: TokenTotals,
    pub open_prs: Vec<OpenPr>,
}

const OPEN_PRS_FILE: &str = "open_prs.json";

impl OrchestratorState {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(StateInner {
                running: HashMap::new(),
                retries: HashMap::new(),
                completed: Vec::new(),
                tokens: TokenTotals::default(),
                open_prs: HashMap::new(),
                state_dir: None,
            })),
        }
    }

    /// Create state with persistence: loads tracked PRs from disk and
    /// persists changes on every `track_pr` / `untrack_pr` / `mark_pr_addressed`.
    pub fn with_persistence(state_dir: PathBuf) -> Self {
        let open_prs = Self::load_open_prs(&state_dir);
        if !open_prs.is_empty() {
            tracing::info!(count = open_prs.len(), "restored tracked PRs from disk");
        }
        Self {
            inner: Arc::new(Mutex::new(StateInner {
                running: HashMap::new(),
                retries: HashMap::new(),
                completed: Vec::new(),
                tokens: TokenTotals::default(),
                open_prs,
                state_dir: Some(state_dir),
            })),
        }
    }

    fn load_open_prs(state_dir: &Path) -> HashMap<String, OpenPr> {
        let file = state_dir.join(OPEN_PRS_FILE);
        let data = match std::fs::read_to_string(&file) {
            Ok(d) => d,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return HashMap::new(),
            Err(e) => {
                tracing::warn!(path = %file.display(), "failed to read persisted PR state: {e}");
                return HashMap::new();
            }
        };
        match serde_json::from_str(&data) {
            Ok(prs) => prs,
            Err(e) => {
                tracing::warn!(path = %file.display(), "failed to parse persisted PR state: {e}");
                HashMap::new()
            }
        }
    }

    fn persist_open_prs(&self) {
        let (state_dir, json) = {
            let inner = self.inner.lock().unwrap();
            let dir = match inner.state_dir {
                Some(ref d) => d.clone(),
                None => return,
            };
            let json = serde_json::to_string_pretty(&inner.open_prs)
                .unwrap_or_else(|_| "{}".to_string());
            (dir, json)
        };
        let file = state_dir.join(OPEN_PRS_FILE);
        if let Err(e) = std::fs::create_dir_all(&state_dir) {
            tracing::warn!(path = %state_dir.display(), "failed to create state dir: {e}");
            return;
        }
        if let Err(e) = std::fs::write(&file, json) {
            tracing::warn!(path = %file.display(), "failed to persist PR state: {e}");
        }
    }

    pub fn is_running(&self, state_key: &str) -> bool {
        self.inner.lock().unwrap().running.contains_key(state_key)
    }

    pub fn is_in_retry(&self, state_key: &str) -> bool {
        self.inner.lock().unwrap().retries.contains_key(state_key)
    }

    pub fn is_completed_successfully(&self, state_key: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|e| e.issue_id == state_key && e.success)
    }

    pub fn running_count(&self) -> usize {
        self.inner.lock().unwrap().running.len()
    }

    pub fn running_count_for_workflow(&self, workflow_id: &str) -> usize {
        self.inner
            .lock()
            .unwrap()
            .running
            .values()
            .filter(|e| e.workflow_id == workflow_id)
            .count()
    }

    pub fn start_session(&self, state_key: &str, issue: Issue, stall_timeout: Duration, workflow_id: &str) {
        let session = LiveSession::new(state_key.to_string());
        let entry = RunningEntry {
            issue,
            session,
            stall_timeout,
            workflow_id: workflow_id.to_string(),
        };
        self.inner
            .lock()
            .unwrap()
            .running
            .insert(state_key.to_string(), entry);
    }

    pub fn mark_worker_done(&self, state_key: &str, success: bool, error: Option<String>) {
        let mut inner = self.inner.lock().unwrap();
        let (attempts, workflow_id) = inner
            .running
            .get(state_key)
            .map(|e| (e.session.attempts.len() as u32, e.workflow_id.clone()))
            .unwrap_or((0, String::new()));
        inner.running.remove(state_key);
        inner.completed.push(CompletedEntry {
            issue_id: state_key.to_string(),
            success,
            error,
            completed_at: Utc::now(),
            attempts,
            workflow_id,
        });
        if inner.completed.len() > MAX_COMPLETED {
            let drain_count = inner.completed.len() - MAX_COMPLETED;
            inner.completed.drain(..drain_count);
        }
    }

    pub fn mark_retry_ready(&self, state_key: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.retries.get_mut(state_key) {
            entry.ready = true;
        }
    }

    pub fn schedule_retry(&self, state_key: &str, attempt: u32, workflow_id: &str) {
        let entry = RetryEntry::new(state_key.to_string(), attempt)
            .with_workflow(workflow_id.to_string());
        self.inner
            .lock()
            .unwrap()
            .retries
            .insert(state_key.to_string(), entry);
    }

    pub fn take_ready_retries_for_workflow(&self, workflow_id: &str) -> Vec<RetryEntry> {
        let mut inner = self.inner.lock().unwrap();
        let ready_keys: Vec<String> = inner
            .retries
            .iter()
            .filter(|(_, e)| e.ready && e.workflow_id == workflow_id)
            .map(|(k, _)| k.clone())
            .collect();
        ready_keys
            .into_iter()
            .filter_map(|k| inner.retries.remove(&k))
            .collect()
    }

    pub fn push_agent_event(
        &self,
        state_key: &str,
        event: super::session::AgentEvent,
    ) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.running.get_mut(state_key) {
            entry.session.push_event(event);
        }
    }

    pub fn update_session_status(
        &self,
        state_key: &str,
        status: super::session::RunStatus,
    ) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.running.get_mut(state_key) {
            entry.session.status = status;
            entry.session.last_activity = Utc::now();
        }
    }


    pub fn track_pr(&self, state_key: &str, issue: Issue, pr_number: u64, workspace_dir: PathBuf, workflow_id: &str) {
        self.inner.lock().unwrap().open_prs.insert(
            state_key.to_string(),
            OpenPr {
                issue,
                pr_number,
                workspace_dir,
                last_addressed_at: None,
                workflow_id: workflow_id.to_string(),
            },
        );
        self.persist_open_prs();
    }

    pub fn untrack_pr(&self, state_key: &str) {
        self.inner.lock().unwrap().open_prs.remove(state_key);
        self.persist_open_prs();
    }

    pub fn open_prs(&self) -> Vec<OpenPr> {
        self.inner
            .lock()
            .unwrap()
            .open_prs
            .values()
            .cloned()
            .collect()
    }

    pub fn mark_pr_addressed(&self, state_key: &str) {
        if let Some(pr) = self.inner.lock().unwrap().open_prs.get_mut(state_key) {
            pr.last_addressed_at = Some(Utc::now());
        }
        self.persist_open_prs();
    }

    pub fn snapshot(&self) -> StateSnapshot {
        let inner = self.inner.lock().unwrap();
        StateSnapshot {
            running: inner.running.values().cloned().collect(),
            retries: inner.retries.keys().cloned().collect(),
            completed: inner.completed.clone(),
            tokens: inner.tokens.clone(),
            open_prs: inner.open_prs.values().cloned().collect(),
        }
    }

    pub fn get_issue_detail(&self, state_key: &str) -> Option<RunningEntry> {
        self.inner.lock().unwrap().running.get(state_key).cloned()
    }

    /// Find sessions with no activity within their per-entry stall timeout.
    pub fn find_stalled_sessions(
        &self,
        now: chrono::DateTime<Utc>,
    ) -> Vec<(String, Duration)> {
        let inner = self.inner.lock().unwrap();
        inner
            .running
            .iter()
            .filter(|(_, entry)| {
                let threshold = chrono::Duration::from_std(entry.stall_timeout)
                    .unwrap_or(chrono::Duration::seconds(300));
                now - entry.session.last_activity > threshold
            })
            .map(|(id, entry)| (id.clone(), entry.stall_timeout))
            .collect()
    }
}

impl Default for OrchestratorState {
    fn default() -> Self {
        Self::new()
    }
}
