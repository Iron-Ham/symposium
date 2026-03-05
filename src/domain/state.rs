use super::issue::Issue;
use super::retry::RetryEntry;
use super::session::LiveSession;
use crate::config::schema::ServiceConfig;
use chrono::{DateTime, Utc};
use serde::Serialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

const MAX_COMPLETED: usize = 50;

#[derive(Debug, Clone)]
pub struct OrchestratorState {
    inner: Arc<Mutex<StateInner>>,
    config_rx: watch::Receiver<ServiceConfig>,
}

#[derive(Debug)]
struct StateInner {
    running: HashMap<String, RunningEntry>,
    retries: HashMap<String, RetryEntry>,
    completed: Vec<CompletedEntry>,
    tokens: TokenTotals,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunningEntry {
    pub issue: Issue,
    pub session: LiveSession,
}

#[derive(Debug, Clone, Serialize)]
pub struct CompletedEntry {
    pub issue_id: String,
    pub success: bool,
    pub error: Option<String>,
    pub completed_at: DateTime<Utc>,
    pub attempts: u32,
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
}

impl OrchestratorState {
    pub fn new(config_rx: watch::Receiver<ServiceConfig>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(StateInner {
                running: HashMap::new(),
                retries: HashMap::new(),
                completed: Vec::new(),
                tokens: TokenTotals::default(),
            })),
            config_rx,
        }
    }

    pub fn config(&self) -> ServiceConfig {
        self.config_rx.borrow().clone()
    }

    pub fn is_running(&self, issue_id: &str) -> bool {
        self.inner.lock().unwrap().running.contains_key(issue_id)
    }

    pub fn is_in_retry(&self, issue_id: &str) -> bool {
        self.inner.lock().unwrap().retries.contains_key(issue_id)
    }

    pub fn is_completed_successfully(&self, issue_id: &str) -> bool {
        self.inner
            .lock()
            .unwrap()
            .completed
            .iter()
            .any(|e| e.issue_id == issue_id && e.success)
    }

    pub fn running_count(&self) -> usize {
        self.inner.lock().unwrap().running.len()
    }

    pub fn start_session(&self, issue: Issue) {
        let id = issue.identifier.clone();
        let session = LiveSession::new(id.clone());
        let entry = RunningEntry { issue, session };
        self.inner.lock().unwrap().running.insert(id, entry);
    }

    pub fn mark_worker_done(&self, issue_id: &str, success: bool, error: Option<String>) {
        let mut inner = self.inner.lock().unwrap();
        let attempts = inner
            .running
            .get(issue_id)
            .map(|e| e.session.attempts.len() as u32)
            .unwrap_or(0);
        inner.running.remove(issue_id);
        inner.completed.push(CompletedEntry {
            issue_id: issue_id.to_string(),
            success,
            error,
            completed_at: Utc::now(),
            attempts,
        });
        if inner.completed.len() > MAX_COMPLETED {
            let drain_count = inner.completed.len() - MAX_COMPLETED;
            inner.completed.drain(..drain_count);
        }
    }

    pub fn mark_retry_ready(&self, issue_id: &str) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.retries.get_mut(issue_id) {
            entry.ready = true;
        }
    }

    pub fn schedule_retry(&self, issue_id: &str, attempt: u32) {
        let entry = RetryEntry::new(issue_id.to_string(), attempt);
        self.inner
            .lock()
            .unwrap()
            .retries
            .insert(issue_id.to_string(), entry);
    }

    pub fn take_ready_retries(&self) -> Vec<RetryEntry> {
        let mut inner = self.inner.lock().unwrap();
        let ready_keys: Vec<String> = inner
            .retries
            .iter()
            .filter(|(_, e)| e.ready)
            .map(|(k, _)| k.clone())
            .collect();
        ready_keys
            .into_iter()
            .filter_map(|k| inner.retries.remove(&k))
            .collect()
    }

    pub fn push_agent_event(
        &self,
        issue_id: &str,
        event: super::session::AgentEvent,
    ) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.running.get_mut(issue_id) {
            entry.session.push_event(event);
        }
    }

    pub fn update_session_status(
        &self,
        issue_id: &str,
        status: super::session::RunStatus,
    ) {
        let mut inner = self.inner.lock().unwrap();
        if let Some(entry) = inner.running.get_mut(issue_id) {
            entry.session.status = status;
            entry.session.last_activity = Utc::now();
        }
    }


    pub fn snapshot(&self) -> StateSnapshot {
        let inner = self.inner.lock().unwrap();
        StateSnapshot {
            running: inner.running.values().cloned().collect(),
            retries: inner.retries.keys().cloned().collect(),
            completed: inner.completed.clone(),
            tokens: inner.tokens.clone(),
        }
    }

    pub fn get_issue_detail(&self, issue_id: &str) -> Option<RunningEntry> {
        self.inner.lock().unwrap().running.get(issue_id).cloned()
    }

    pub fn find_stalled_sessions(
        &self,
        now: chrono::DateTime<Utc>,
        stall_timeout: std::time::Duration,
    ) -> Vec<String> {
        let inner = self.inner.lock().unwrap();
        let threshold = chrono::Duration::from_std(stall_timeout).unwrap_or(chrono::Duration::seconds(300));
        inner
            .running
            .iter()
            .filter(|(_, entry)| now - entry.session.last_activity > threshold)
            .map(|(id, _)| id.clone())
            .collect()
    }
}
