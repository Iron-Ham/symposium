pub mod mcp;
pub mod mcp_http;
pub mod notion;
pub mod oauth;
pub mod sentry;

use crate::domain::issue::{Comment, Issue};
use crate::error::Result;

/// Trait for issue tracker backends.
#[allow(async_fn_in_trait)]
pub trait TrackerClient {
    /// Fetch issues in active states (candidates for agent work).
    async fn fetch_candidate_issues(&mut self) -> Result<Vec<Issue>>;

    /// Fetch current state for specific issue IDs (for reconciliation).
    async fn fetch_issue_states_by_ids(&mut self, ids: &[String]) -> Result<Vec<Issue>>;

    /// Fetch issues in terminal states (for cleanup).
    async fn fetch_terminal_issues(&mut self) -> Result<Vec<Issue>>;

    /// Execute an arbitrary query on behalf of an agent.
    async fn agent_query(&mut self, sql: &str) -> Result<serde_json::Value>;

    /// Fetch comments on an issue page. Returns empty vec by default.
    async fn fetch_comments(&mut self, _page_id: &str) -> Result<Vec<Comment>> {
        Ok(vec![])
    }

    /// Update the status of an issue in the tracker. Default no-op.
    async fn update_issue_status(
        &mut self,
        _page_id: &str,
        _status_property: &str,
        _status_value: &str,
    ) -> Result<()> {
        Ok(())
    }

    /// Fetch ALL sub-tasks of an epic regardless of status (for dependency graph).
    async fn fetch_all_epic_tasks(&mut self) -> Result<Vec<Issue>> {
        Ok(vec![])
    }
}
