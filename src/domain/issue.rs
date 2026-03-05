use serde::{Deserialize, Serialize};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Blocker {
    pub identifier: String,
    pub title: String,
    pub status: String,
}
