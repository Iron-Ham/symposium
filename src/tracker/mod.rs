pub mod mcp;
pub mod mcp_http;
pub mod notion;
pub mod oauth;
pub mod sentry;

use crate::domain::issue::Issue;
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
}
