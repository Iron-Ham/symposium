use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSession {
    pub issue_id: String,
    pub thread_id: Option<String>,
    pub current_turn: u32,
    pub status: RunStatus,
    pub started_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub attempts: Vec<RunAttempt>,
}

impl LiveSession {
    pub fn new(issue_id: String) -> Self {
        let now = Utc::now();
        Self {
            issue_id,
            thread_id: None,
            current_turn: 0,
            status: RunStatus::Starting,
            started_at: now,
            last_activity: now,
            attempts: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunAttempt {
    pub attempt_number: u32,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub status: RunStatus,
    pub error: Option<String>,
    pub turns_used: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RunStatus {
    Starting,
    Running,
    WaitingForTool,
    Completed,
    Failed,
    Stalled,
    Cancelled,
}
