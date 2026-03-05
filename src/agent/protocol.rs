use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC 2.0 request.
#[derive(Debug, Serialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: &'static str,
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcRequest {
    pub fn new(id: u64, method: impl Into<String>, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0", id, method: method.into(), params }
    }
}

/// A JSON-RPC 2.0 notification (no id).
#[derive(Debug, Serialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: &'static str,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl JsonRpcNotification {
    pub fn new(method: impl Into<String>, params: Option<Value>) -> Self {
        Self { jsonrpc: "2.0", method: method.into(), params }
    }
}

/// A JSON-RPC 2.0 response.
#[derive(Debug, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    pub id: Option<u64>,
    pub result: Option<Value>,
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Deserialize)]
pub struct JsonRpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

/// Agent turn events streamed from the agent server.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type")]
pub enum TurnEvent {
    #[serde(rename = "text_delta")]
    TextDelta { delta: String },
    #[serde(rename = "tool_call")]
    ToolCall { id: String, name: String, arguments: Value },
    #[serde(rename = "tool_result")]
    ToolResult { id: String, content: Value },
    #[serde(rename = "turn_complete")]
    TurnComplete { turn_id: String },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(other)]
    Unknown,
}

/// Result of a completed turn.
#[derive(Debug)]
pub enum TurnResult {
    Complete,
    NeedsToolResponse { tool_call_id: String, tool_name: String, arguments: Value },
    Error(String),
    AgentExited,
}
