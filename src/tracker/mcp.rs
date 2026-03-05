use crate::error::{Error, Result};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub struct McpClient {
    child: Child,
    stdin: BufWriter<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_id: AtomicU64,
}

impl McpClient {
    /// Spawn the MCP server process and initialize connection.
    pub async fn new(command: &str, args: &[&str]) -> Result<Self> {
        let (cmd, effective_args): (&str, Vec<&str>) = if args.is_empty() {
            let parts: Vec<&str> = command.split_whitespace().collect();
            let (first, rest) = parts
                .split_first()
                .ok_or_else(|| Error::Mcp("empty command".into()))?;
            (*first, rest.to_vec())
        } else {
            (command, args.to_vec())
        };

        let mut child = Command::new(cmd)
            .args(&effective_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .spawn()
            .map_err(|e| Error::Mcp(format!("failed to spawn MCP server: {e}")))?;

        let stdin = BufWriter::new(
            child
                .stdin
                .take()
                .ok_or_else(|| Error::Mcp("no stdin on MCP child".into()))?,
        );
        let stdout = BufReader::new(
            child
                .stdout
                .take()
                .ok_or_else(|| Error::Mcp("no stdout on MCP child".into()))?,
        );

        let mut client = Self {
            child,
            stdin,
            stdout,
            next_id: AtomicU64::new(1),
        };

        // Initialize MCP connection
        let _init_result = client
            .send_request(
                "initialize",
                serde_json::json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": {},
                    "clientInfo": {
                        "name": "symposium",
                        "version": env!("CARGO_PKG_VERSION")
                    }
                }),
            )
            .await?;

        client
            .send_notification("notifications/initialized", serde_json::json!({}))
            .await?;

        Ok(client)
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

    /// Send a JSON-RPC request and read the response.
    async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        });

        let mut line = serde_json::to_string(&request)
            .map_err(|e| Error::Mcp(format!("serialize error: {e}")))?;
        line.push('\n');

        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| Error::Mcp(format!("write error: {e}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| Error::Mcp(format!("flush error: {e}")))?;

        // Read lines until we get a response with matching id
        loop {
            let mut response_line = String::new();
            let n = self
                .stdout
                .read_line(&mut response_line)
                .await
                .map_err(|e| Error::Mcp(format!("read error: {e}")))?;
            if n == 0 {
                return Err(Error::Mcp("MCP server closed stdout".into()));
            }

            let trimmed = response_line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let response: Value = serde_json::from_str(trimmed)
                .map_err(|e| Error::Mcp(format!("parse error: {e}")))?;

            // Skip notifications (no id field)
            if response.get("id").and_then(|v| v.as_u64()) == Some(id) {
                if let Some(error) = response.get("error") {
                    return Err(Error::Mcp(format!("MCP error: {error}")));
                }
                return Ok(response.get("result").cloned().unwrap_or(Value::Null));
            }
        }
    }

    /// Send a JSON-RPC notification (no response expected).
    async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        });
        let mut line = serde_json::to_string(&notification)
            .map_err(|e| Error::Mcp(format!("serialize error: {e}")))?;
        line.push('\n');
        self.stdin
            .write_all(line.as_bytes())
            .await
            .map_err(|e| Error::Mcp(format!("write error: {e}")))?;
        self.stdin
            .flush()
            .await
            .map_err(|e| Error::Mcp(format!("flush error: {e}")))?;
        Ok(())
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
