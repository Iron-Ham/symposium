use super::process::AgentProcess;
use super::protocol::*;
use super::tools;
use crate::config::schema::ServiceConfig;
use crate::error::{Error, Result};
use serde_json::{json, Value};

pub struct AgentWorker {
    process: AgentProcess,
    issue_id: String,
    thread_id: Option<String>,
    turn_id: Option<String>,
    next_id: u64,
}

impl AgentWorker {
    pub fn new(process: AgentProcess, issue_id: String) -> Self {
        Self {
            process,
            issue_id,
            thread_id: None,
            turn_id: None,
            next_id: 1,
        }
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    /// Perform the initialize handshake.
    pub async fn initialize(&mut self, _config: &ServiceConfig) -> Result<()> {
        let req = JsonRpcRequest::new(
            self.next_request_id(),
            "initialize",
            Some(json!({
                "protocolVersion": "2024-01-01",
                "capabilities": {},
                "clientInfo": {
                    "name": "symposium",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
        );
        self.process
            .send(&serde_json::to_value(&req).unwrap())
            .await?;

        let resp = self.read_response(req.id).await?;
        tracing::debug!(issue_id = %self.issue_id, "agent initialized: {resp:?}");

        let notif = JsonRpcNotification::new("initialized", Some(json!({})));
        self.process
            .send(&serde_json::to_value(&notif).unwrap())
            .await?;

        Ok(())
    }

    /// Start a new thread with the given prompt.
    pub async fn start_thread(&mut self, prompt: &str) -> Result<()> {
        let req = JsonRpcRequest::new(
            self.next_request_id(),
            "thread/start",
            Some(json!({
                "instructions": prompt
            })),
        );
        self.process
            .send(&serde_json::to_value(&req).unwrap())
            .await?;

        let resp = self.read_response(req.id).await?;
        self.thread_id = resp
            .get("threadId")
            .and_then(|v| v.as_str())
            .map(String::from);

        tracing::info!(issue_id = %self.issue_id, thread_id = ?self.thread_id, "thread started");
        Ok(())
    }

    /// Run a single turn. Returns the turn result.
    pub async fn run_turn(&mut self, prompt: &str) -> Result<TurnResult> {
        let thread_id = self
            .thread_id
            .clone()
            .ok_or_else(|| Error::AgentProtocol("no thread_id".into()))?;

        let req = JsonRpcRequest::new(
            self.next_request_id(),
            "turn/start",
            Some(json!({
                "threadId": thread_id,
                "message": prompt
            })),
        );
        self.process
            .send(&serde_json::to_value(&req).unwrap())
            .await?;

        loop {
            let msg = match self.process.recv().await? {
                Some(msg) => msg,
                None => return Ok(TurnResult::AgentExited),
            };

            if let Some(id) = msg.get("id").and_then(|v| v.as_u64())
                && id == req.id {
                    self.turn_id = msg
                        .get("result")
                        .and_then(|r| r.get("turnId"))
                        .and_then(|v| v.as_str())
                        .map(String::from);
                    continue;
                }

            if let Some(method) = msg.get("method").and_then(|v| v.as_str())
                && method == "turn/event"
                    && let Some(params) = msg.get("params") {
                        match serde_json::from_value::<TurnEvent>(params.clone()) {
                            Ok(TurnEvent::TurnComplete { .. }) => {
                                return Ok(TurnResult::Complete);
                            }
                            Ok(TurnEvent::ToolCall {
                                id,
                                name,
                                arguments,
                            }) => {
                                if tools::is_client_tool(&name) {
                                    return Ok(TurnResult::NeedsToolResponse {
                                        tool_call_id: id,
                                        tool_name: name,
                                        arguments,
                                    });
                                }
                            }
                            Ok(TurnEvent::Error { message }) => {
                                return Ok(TurnResult::Error(message));
                            }
                            Ok(TurnEvent::TextDelta { delta }) => {
                                tracing::trace!(issue_id = %self.issue_id, "text: {delta}");
                            }
                            Ok(_) => {}
                            Err(e) => {
                                tracing::warn!("failed to parse turn event: {e}");
                            }
                        }
                    }
        }
    }

    /// Send a tool response back to the agent.
    pub async fn send_tool_response(&mut self, tool_call_id: &str, result: Value) -> Result<()> {
        let req = JsonRpcRequest::new(
            self.next_request_id(),
            "turn/toolResponse",
            Some(json!({
                "toolCallId": tool_call_id,
                "content": result
            })),
        );
        self.process
            .send(&serde_json::to_value(&req).unwrap())
            .await?;
        Ok(())
    }

    /// Read a JSON-RPC response with the given ID, skipping notifications.
    async fn read_response(&mut self, expected_id: u64) -> Result<Value> {
        loop {
            let msg = self
                .process
                .recv()
                .await?
                .ok_or_else(|| {
                    Error::AgentProtocol("agent process exited during handshake".into())
                })?;

            if let Some(id) = msg.get("id").and_then(|v| v.as_u64())
                && id == expected_id {
                    if let Some(error) = msg.get("error") {
                        return Err(Error::AgentProtocol(format!("agent error: {error}")));
                    }
                    return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
                }
        }
    }

    pub async fn kill(&mut self) -> Result<()> {
        self.process.kill().await
    }
}

/// Run a full agent attempt: multiple turns until completion or max_turns.
pub async fn run_agent_attempt(
    worker: &mut AgentWorker,
    initial_prompt: &str,
    max_turns: u32,
) -> Result<bool> {
    let mut turn = 0u32;
    let mut prompt = initial_prompt.to_string();

    loop {
        if turn >= max_turns {
            tracing::warn!("max turns ({max_turns}) reached");
            return Ok(false);
        }

        let result = worker.run_turn(&prompt).await?;
        turn += 1;

        match result {
            TurnResult::Complete => {
                tracing::info!(turn, "agent completed successfully");
                return Ok(true);
            }
            TurnResult::NeedsToolResponse {
                tool_call_id,
                tool_name,
                arguments,
            } => {
                let tool_result = tools::handle_tool_call(&tool_name, &arguments).await?;
                worker.send_tool_response(&tool_call_id, tool_result).await?;
                prompt = "Continue with the tool result.".to_string();
            }
            TurnResult::Error(msg) => {
                tracing::error!(turn, "agent error: {msg}");
                return Err(Error::Agent(msg));
            }
            TurnResult::AgentExited => {
                tracing::warn!(turn, "agent process exited unexpectedly");
                return Err(Error::Agent("agent process exited".into()));
            }
        }
    }
}
