use super::process::AgentProcess;
use crate::config::schema::ServiceConfig;
use crate::error::{Error, Result};

pub struct AgentWorker {
    pub(crate) process: AgentProcess,
    #[allow(dead_code)]
    issue_id: String,
}

impl AgentWorker {
    pub fn new(process: AgentProcess, issue_id: String) -> Self {
        Self { process, issue_id }
    }

    /// No-op for stream-json mode — initialization happens at spawn.
    pub async fn initialize(&mut self, _config: &ServiceConfig) -> Result<()> {
        Ok(())
    }

    /// No-op — the prompt is passed via stdin at spawn time.
    pub async fn start_thread(&mut self, _prompt: &str) -> Result<()> {
        Ok(())
    }

    pub async fn kill(&mut self) -> Result<()> {
        self.process.kill().await
    }
}

/// Result of a streamed event from the agent.
#[derive(Debug)]
pub enum StreamEvent {
    /// Agent sent a text message
    AssistantText(String),
    /// Agent is using a tool
    ToolUse { name: String, input: String },
    /// Agent finished successfully
    Result { result: String, cost_usd: f64, num_turns: u64 },
    /// Agent errored
    Error(String),
    /// Process exited
    Eof,
}

/// Run the agent to completion, streaming events back to state.
pub async fn run_agent_attempt(
    worker: &mut AgentWorker,
    _initial_prompt: &str,
    state: &crate::domain::state::OrchestratorState,
    issue_id: &str,
) -> Result<bool> {
    use crate::domain::session::{AgentEvent, AgentEventKind};

    loop {
        let msg = match worker.process.recv().await? {
            Some(msg) => msg,
            None => {
                state.push_agent_event(
                    issue_id,
                    AgentEvent::now(AgentEventKind::Error {
                        message: "agent process exited".into(),
                    }),
                );
                return Err(Error::Agent("agent process exited unexpectedly".into()));
            }
        };

        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");

        match msg_type {
            "system" => {
                let subtype = msg.get("subtype").and_then(|v| v.as_str()).unwrap_or("");
                if subtype == "init" {
                    state.push_agent_event(
                        issue_id,
                        AgentEvent::now(AgentEventKind::Status {
                            status: "Agent initialized".into(),
                        }),
                    );
                    state.update_session_status(
                        issue_id,
                        crate::domain::session::RunStatus::Running,
                    );
                }
            }
            "assistant" => {
                if let Some(message) = msg.get("message")
                    && let Some(content) = message.get("content").and_then(|v| v.as_array())
                {
                    for block in content {
                        let block_type =
                            block.get("type").and_then(|v| v.as_str()).unwrap_or("");
                        match block_type {
                            "text" => {
                                if let Some(text) = block.get("text").and_then(|v| v.as_str())
                                {
                                    let preview: String = text.chars().take(300).collect();
                                    state.push_agent_event(
                                        issue_id,
                                        AgentEvent::now(AgentEventKind::Text {
                                            text: preview,
                                        }),
                                    );
                                }
                            }
                            "tool_use" => {
                                let name = block
                                    .get("name")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("unknown");
                                let input = block
                                    .get("input")
                                    .map(|v| v.to_string())
                                    .unwrap_or_default();
                                let truncated: String = input.chars().take(200).collect();
                                state.push_agent_event(
                                    issue_id,
                                    AgentEvent::now(AgentEventKind::ToolCall {
                                        name: name.to_string(),
                                        arguments: truncated,
                                    }),
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
            "result" => {
                let is_error = msg.get("is_error").and_then(|v| v.as_bool()).unwrap_or(false);
                let result_text = msg
                    .get("result")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let cost = msg
                    .get("total_cost_usd")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(0.0);
                let num_turns = msg
                    .get("num_turns")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);

                if is_error {
                    state.push_agent_event(
                        issue_id,
                        AgentEvent::now(AgentEventKind::Error {
                            message: result_text.clone(),
                        }),
                    );
                    return Err(Error::Agent(result_text));
                }

                state.push_agent_event(
                    issue_id,
                    AgentEvent::now(AgentEventKind::TurnComplete {
                        turn: num_turns as u32,
                    }),
                );
                state.push_agent_event(
                    issue_id,
                    AgentEvent::now(AgentEventKind::Status {
                        status: format!(
                            "Completed in {num_turns} turns (${:.4})",
                            cost
                        ),
                    }),
                );

                tracing::info!(
                    issue_id,
                    num_turns,
                    cost_usd = cost,
                    "agent completed"
                );
                return Ok(true);
            }
            _ => {
                // Skip unknown event types (hook events, etc.)
            }
        }
    }
}
