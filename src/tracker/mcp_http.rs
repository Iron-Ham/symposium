use super::oauth::OAuthClient;
use crate::error::{Error, Result};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};

/// MCP client that communicates via Streamable HTTP transport.
pub struct HttpMcpClient {
    url: String,
    http: reqwest::Client,
    oauth: OAuthClient,
    next_id: AtomicU64,
    session_id: Option<String>,
}

impl HttpMcpClient {
    pub async fn new(url: &str) -> Result<Self> {
        let oauth = OAuthClient::new(url);
        let mut client = Self {
            url: url.trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
            oauth,
            next_id: AtomicU64::new(1),
            session_id: None,
        };

        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let _result = self
            .send_request(
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2025-03-26",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "symposium",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;

        self.send_notification("notifications/initialized", serde_json::json!({}))
            .await?;

        Ok(())
    }

    /// Call a tool on the MCP server.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<Value> {
        self.send_request(
            "tools/call",
            serde_json::json!({
                "name": name,
                "arguments": args
            }),
        )
        .await
    }

    async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let response = self.post_json(&body).await?;

        // Extract Mcp-Session-Id from response headers
        if let Some(sid) = response.headers().get("mcp-session-id")
            && let Ok(s) = sid.to_str()
        {
            self.session_id = Some(s.to_string());
        }

        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            // Token might be stale — force re-auth and retry once
            tracing::info!("MCP returned 401, re-authenticating...");
            self.oauth = OAuthClient::new(&self.url);
            let response = self.post_json(&body).await?;
            return self.parse_response(response, id).await;
        }

        self.parse_response(response, id).await
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        let _ = self.post_json(&body).await?;
        Ok(())
    }

    async fn post_json(&mut self, body: &Value) -> Result<reqwest::Response> {
        let token = self.oauth.get_token().await?;

        let mut req = self
            .http
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream")
            .bearer_auth(&token);

        if let Some(ref sid) = self.session_id {
            req = req.header("Mcp-Session-Id", sid);
        }

        req.json(body)
            .send()
            .await
            .map_err(|e| Error::Mcp(format!("HTTP request failed: {e}")))
    }

    async fn parse_response(&self, response: reqwest::Response, id: u64) -> Result<Value> {
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(Error::Mcp(format!("MCP HTTP error {status}: {body}")));
        }

        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();

        let text = response
            .text()
            .await
            .map_err(|e| Error::Mcp(format!("read response body: {e}")))?;

        if content_type.contains("text/event-stream") {
            self.parse_sse_response(&text, id)
        } else {
            self.parse_json_response(&text, id)
        }
    }

    /// Parse a plain JSON or newline-delimited JSON response.
    fn parse_json_response(&self, text: &str, id: u64) -> Result<Value> {
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Ok(msg) = serde_json::from_str::<Value>(trimmed)
                && msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                    if let Some(error) = msg.get("error") {
                        return Err(Error::Mcp(format!("MCP error: {error}")));
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
        }
        Err(Error::Mcp("no matching response from MCP server".into()))
    }

    /// Parse a Server-Sent Events (SSE) response.
    /// SSE format: lines starting with "data:" contain JSON-RPC messages.
    fn parse_sse_response(&self, text: &str, id: u64) -> Result<Value> {
        for line in text.lines() {
            let trimmed = line.trim();

            // SSE data lines start with "data:"
            let json_str = if let Some(data) = trimmed.strip_prefix("data:") {
                data.trim()
            } else if trimmed.starts_with('{') {
                // Some servers send raw JSON between SSE events
                trimmed
            } else {
                continue;
            };

            if json_str.is_empty() {
                continue;
            }

            if let Ok(msg) = serde_json::from_str::<Value>(json_str)
                && msg.get("id").and_then(|v| v.as_u64()) == Some(id) {
                    if let Some(error) = msg.get("error") {
                        return Err(Error::Mcp(format!("MCP error: {error}")));
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
        }
        Err(Error::Mcp("no matching response in SSE stream".into()))
    }
}
