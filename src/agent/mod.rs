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
    pub async fn start_session(
        &self,
        workspace_dir: &Path,
        prompt: &str,
        issue_id: &str,
    ) -> Result<worker::AgentWorker> {
        let proc = process::AgentProcess::spawn(
            &self.config.codex.command,
            workspace_dir,
        )
        .await?;

        let mut w = worker::AgentWorker::new(proc, issue_id.to_string());
        w.initialize(&self.config).await?;
        w.start_thread(prompt).await?;
        Ok(w)
    }
}
