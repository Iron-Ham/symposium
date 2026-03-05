pub mod process;
pub mod protocol;
pub mod tools;
pub mod worker;

use crate::config::schema::ServiceConfig;
use crate::error::Result;
use std::path::Path;

pub struct AgentRunner {
    config: ServiceConfig,
}

impl AgentRunner {
    pub fn new(config: ServiceConfig) -> Self {
        Self { config }
    }

    /// Start a new agent session in the given workspace directory.
    /// Spawns `claude -p --output-format stream-json --verbose` with the prompt on stdin.
    pub async fn start_session(
        &self,
        workspace_dir: &Path,
        prompt: &str,
        issue_id: &str,
    ) -> Result<worker::AgentWorker> {
        let cmd = format!(
            "{} -p --output-format stream-json --verbose --no-session-persistence --dangerously-skip-permissions",
            self.config.codex.command
        );

        let mut proc = process::AgentProcess::spawn(&cmd, workspace_dir).await?;

        // Write prompt to stdin and close it — claude -p reads until EOF
        proc.write_and_close_stdin(prompt).await?;

        Ok(worker::AgentWorker::new(proc, issue_id.to_string()))
    }
}
