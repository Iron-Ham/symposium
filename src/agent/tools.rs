use crate::error::{Error, Result};
use crate::tracker::TrackerClient;
use crate::tracker::notion::NotionTracker;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Known client-side tools that we handle rather than the agent.
const CLIENT_TOOLS: &[&str] = &["notion_query"];

pub fn is_client_tool(name: &str) -> bool {
    CLIENT_TOOLS.contains(&name)
}

/// Shared tracker handle for agent tool calls.
pub type TrackerHandle = Arc<Mutex<Option<NotionTracker>>>;

/// Handle a client-side tool call (without tracker — returns stub error).
pub async fn handle_tool_call(name: &str, arguments: &Value) -> Result<Value> {
    match name {
        "notion_query" => {
            let sql = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .unwrap_or("<unknown>");
            tracing::warn!("notion_query called without tracker context: {sql}");
            Ok(serde_json::json!({
                "error": "notion_query requires tracker context"
            }))
        }
        _ => Err(Error::Agent(format!("unknown client tool: {name}"))),
    }
}

/// Handle a tool call with access to a tracker instance.
pub async fn handle_tool_call_with_tracker(
    name: &str,
    arguments: &Value,
    tracker: &TrackerHandle,
) -> Result<Value> {
    match name {
        "notion_query" => {
            let sql = arguments
                .get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| Error::Agent("notion_query: missing 'query' argument".into()))?;

            let mut guard = tracker.lock().await;
            if let Some(ref mut t) = *guard {
                t.agent_query(sql).await
            } else {
                Ok(serde_json::json!({
                    "error": "tracker not available"
                }))
            }
        }
        _ => Err(Error::Agent(format!("unknown client tool: {name}"))),
    }
}
