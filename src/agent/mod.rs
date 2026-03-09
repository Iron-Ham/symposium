pub mod process;
pub mod protocol;
pub mod tools;
pub mod worker;

use crate::config::schema::{McpServerConfig, ServiceConfig};
use crate::error::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// RAII guard that deletes the temporary MCP config file on drop.
pub struct TempMcpConfig {
    path: PathBuf,
}

impl Drop for TempMcpConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Write an MCP config JSON file suitable for `claude --mcp-config <path>`.
///
/// Returns a `TempMcpConfig` guard that cleans up the file when dropped.
/// Monotonic counter to ensure unique temp file names across concurrent calls.
static MCP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn write_mcp_config(
    servers: &HashMap<String, McpServerConfig>,
    issue_id: &str,
) -> Result<TempMcpConfig> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let wrapper = serde_json::json!({ "mcpServers": servers });
    let seq = MCP_FILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "symposium-mcp-{}-{}-{}.json",
        issue_id,
        std::process::id(),
        seq,
    ));
    let contents = serde_json::to_string_pretty(&wrapper)
        .map_err(|e| crate::error::Error::Agent(e.to_string()))?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&path)
        .map_err(|e| crate::error::Error::Agent(e.to_string()))?;
    file.write_all(contents.as_bytes())
        .map_err(|e| crate::error::Error::Agent(e.to_string()))?;
    Ok(TempMcpConfig { path })
}

pub struct AgentRunner {
    config: ServiceConfig,
}

impl AgentRunner {
    pub fn new(config: ServiceConfig) -> Self {
        Self { config }
    }

    /// Start a new agent session in the given workspace directory.
    /// Spawns `claude -p --output-format stream-json --verbose` with the prompt on stdin.
    ///
    /// Returns the worker and an optional `TempMcpConfig` guard. The caller must
    /// hold the guard alive until the agent finishes so the temp file isn't deleted early.
    pub async fn start_session(
        &self,
        workspace_dir: &Path,
        prompt: &str,
        issue_id: &str,
    ) -> Result<(worker::AgentWorker, Option<TempMcpConfig>)> {
        let mut cmd = format!(
            "{} -p --output-format stream-json --verbose --no-session-persistence --dangerously-skip-permissions",
            self.config.codex.command
        );

        let mcp_guard = if !self.config.mcp_servers.is_empty() {
            let guard = write_mcp_config(&self.config.mcp_servers, issue_id)?;
            cmd.push_str(&format!(" --mcp-config \"{}\"", guard.path.display()));
            Some(guard)
        } else {
            None
        };

        let mut proc = process::AgentProcess::spawn(&cmd, workspace_dir).await?;

        // Write prompt to stdin and close it — claude -p reads until EOF
        proc.write_and_close_stdin(prompt).await?;

        Ok((worker::AgentWorker::new(proc, issue_id.to_string()), mcp_guard))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::McpServerConfig;

    #[test]
    fn write_mcp_config_produces_valid_json() {
        let mut servers = HashMap::new();
        servers.insert(
            "sentry".to_string(),
            McpServerConfig {
                server_type: "http".to_string(),
                url: Some("https://mcp.sentry.dev/mcp".to_string()),
                ..Default::default()
            },
        );
        servers.insert(
            "linter".to_string(),
            McpServerConfig {
                server_type: "stdio".to_string(),
                command: Some("npx".to_string()),
                args: Some(vec!["-y".to_string(), "@my-org/linter-mcp".to_string()]),
                env: Some(HashMap::from([("API_KEY".to_string(), "secret".to_string())])),
                ..Default::default()
            },
        );

        let guard = write_mcp_config(&servers, "TEST-1").unwrap();
        let contents = std::fs::read_to_string(&guard.path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();

        let mcp_servers = parsed.get("mcpServers").expect("mcpServers key");
        let sentry = mcp_servers.get("sentry").expect("sentry server");
        assert_eq!(sentry["type"], "http");
        assert_eq!(sentry["url"], "https://mcp.sentry.dev/mcp");

        let linter = mcp_servers.get("linter").expect("linter server");
        assert_eq!(linter["type"], "stdio");
        assert_eq!(linter["command"], "npx");
        assert_eq!(linter["args"][0], "-y");
        assert_eq!(linter["env"]["API_KEY"], "secret");
    }

    #[test]
    fn temp_mcp_config_deletes_on_drop() {
        let mut servers = HashMap::new();
        servers.insert(
            "test".to_string(),
            McpServerConfig {
                server_type: "http".to_string(),
                url: Some("https://example.com".to_string()),
                ..Default::default()
            },
        );

        let guard = write_mcp_config(&servers, "DROP-TEST").unwrap();
        let path = guard.path.clone();
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }
}
