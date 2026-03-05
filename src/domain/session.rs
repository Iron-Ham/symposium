use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const MAX_EVENTS: usize = 200;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveSession {
    pub issue_id: String,
    pub thread_id: Option<String>,
    pub current_turn: u32,
    pub status: RunStatus,
    pub started_at: DateTime<Utc>,
    pub last_activity: DateTime<Utc>,
    pub attempts: Vec<RunAttempt>,
    pub events: Vec<AgentEvent>,
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
            events: Vec::new(),
        }
    }

    pub fn push_event(&mut self, event: AgentEvent) {
        self.last_activity = Utc::now();
        self.events.push(event);
        if self.events.len() > MAX_EVENTS {
            self.events.drain(..self.events.len() - MAX_EVENTS);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentEvent {
    pub timestamp: DateTime<Utc>,
    pub kind: AgentEventKind,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentEventKind {
    #[serde(rename = "status")]
    Status { status: String },
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_call")]
    ToolCall { name: String, arguments: String },
    #[serde(rename = "tool_result")]
    ToolResult { name: String, truncated: String },
    #[serde(rename = "turn_complete")]
    TurnComplete { turn: u32 },
    #[serde(rename = "error")]
    Error { message: String },
}

impl AgentEvent {
    pub fn now(kind: AgentEventKind) -> Self {
        Self {
            timestamp: Utc::now(),
            kind,
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
