use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    /// Unique identifier from the ID property, e.g. "TASK-123456"
    pub identifier: String,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub priority: Option<String>,
    pub url: Option<String>,
    pub notion_page_id: Option<String>,
    pub blockers: Vec<Blocker>,
    /// Which tracker produced this issue: "notion" or "sentry".
    #[serde(default = "default_source")]
    pub source: String,
    /// Extra properties extracted from the tracker (e.g. "platform", "severity").
    #[serde(default)]
    pub extra: HashMap<String, String>,
    /// Comments on the issue page, fetched before dispatch.
    #[serde(default)]
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Comment {
    pub author: String,
    pub body: String,
    pub created_at: Option<String>,
}

fn default_source() -> String {
    "notion".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blocker {
    pub identifier: String,
    pub title: String,
    pub status: String,
}
