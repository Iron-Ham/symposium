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

/// Write an MCP config JSON file suitable for `claude --strict-mcp-config --mcp-config <path>`.
///
/// Merges workflow-level `mcp_servers` with the workspace's `.mcp.json` (if present),
/// converting any HTTP/SSE/URL-type servers to stdio via `npx mcp-remote`. This is
/// necessary because `--mcp-config` + `--strict-mcp-config` replaces all project-level
/// configs, and the `--mcp-config` schema rejects `type: "http"` entries.
///
/// Returns a `TempMcpConfig` guard that cleans up the file when dropped.
static MCP_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Convert a single MCP server JSON value to a stdio-compatible format.
/// HTTP/SSE/URL servers get wrapped with `npx mcp-remote <url>`.
fn normalize_mcp_entry(name: &str, val: &serde_json::Value) -> Option<serde_json::Value> {
    let server_type = val
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("stdio");

    match server_type {
        "http" | "sse" | "url" | "streamable-http" => {
            let url = val.get("url").and_then(|u| u.as_str()).unwrap_or_default();
            if url.is_empty() {
                tracing::warn!(server = name, "MCP server has type \"{server_type}\" but no URL — skipping");
                return None;
            }
            Some(serde_json::json!({
                "command": "npx",
                "args": ["-y", "mcp-remote", url]
            }))
        }
        _ => Some(val.clone()),
    }
}

pub fn write_mcp_config(
    servers: &HashMap<String, McpServerConfig>,
    issue_id: &str,
    workspace_dir: &Path,
) -> Result<TempMcpConfig> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut merged: HashMap<String, serde_json::Value> = HashMap::new();

    // 1. Read the workspace's .mcp.json (walk up to git root)
    if let Some(project_servers) = read_project_mcp_json(workspace_dir) {
        for (name, val) in project_servers {
            if let Some(normalized) = normalize_mcp_entry(&name, &val) {
                merged.insert(name, normalized);
            }
        }
    }

    // 2. Layer workflow-level servers on top (they take priority)
    for (name, config) in servers {
        let server_type = config.server_type.as_str();
        let val = match server_type {
            "http" | "sse" | "url" | "streamable-http" => {
                let url = config.url.as_deref().unwrap_or_default();
                if url.is_empty() {
                    tracing::warn!(server = %name, "workflow MCP server has type \"{server_type}\" but no URL — skipping");
                    continue;
                }
                serde_json::json!({
                    "command": "npx",
                    "args": ["-y", "mcp-remote", url]
                })
            }
            _ => {
                match serde_json::to_value(config) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!(server = %name, "failed to serialize MCP server config: {e} — skipping");
                        continue;
                    }
                }
            }
        };
        merged.insert(name.clone(), val);
    }

    let wrapper = serde_json::json!({ "mcpServers": merged });
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
    tracing::debug!(path = %path.display(), servers = merged.len(), "wrote MCP config:\n{contents}");
    Ok(TempMcpConfig { path })
}

/// Read `.mcp.json` from the workspace directory or any parent (up to filesystem root).
/// Returns the `mcpServers` map if found.
fn read_project_mcp_json(workspace_dir: &Path) -> Option<HashMap<String, serde_json::Value>> {
    let mut dir = workspace_dir;
    loop {
        let candidate = dir.join(".mcp.json");
        if candidate.is_file()
            && let Ok(contents) = std::fs::read_to_string(&candidate)
            && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&contents)
            && let Some(servers) = parsed.get("mcpServers").and_then(|s| s.as_object())
        {
            tracing::debug!(
                path = %candidate.display(),
                count = servers.len(),
                "merging project .mcp.json"
            );
            return Some(
                servers
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            );
        }
        match dir.parent() {
            Some(p) if p != dir => dir = p,
            _ => break,
        }
    }
    None
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

        // When workflow defines mcp_servers, we merge them with the workspace's
        // .mcp.json (converting HTTP servers to stdio), then use --strict-mcp-config
        // to replace the project config entirely with our sanitized version.
        let mcp_guard = if !self.config.mcp_servers.is_empty() {
            let guard = write_mcp_config(&self.config.mcp_servers, issue_id, workspace_dir)?;
            cmd.push_str(&format!(
                " --strict-mcp-config --mcp-config {}",
                guard.path.display()
            ));
            Some(guard)
        } else {
            None
        };

        tracing::info!(issue_id, cwd = %workspace_dir.display(), "spawning agent: {cmd}");

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

        let guard = write_mcp_config(&servers, "TEST-1", Path::new("/tmp")).unwrap();
        let contents = std::fs::read_to_string(&guard.path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&contents).unwrap();

        let mcp_servers = parsed.get("mcpServers").expect("mcpServers key");

        // HTTP servers should be wrapped as stdio via mcp-remote
        let sentry = mcp_servers.get("sentry").expect("sentry server");
        assert_eq!(sentry["command"], "npx");
        assert_eq!(sentry["args"][0], "-y");
        assert_eq!(sentry["args"][1], "mcp-remote");
        assert_eq!(sentry["args"][2], "https://mcp.sentry.dev/mcp");
        assert!(sentry.get("type").is_none(), "HTTP servers should not have a type field");

        // stdio servers remain unchanged
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

        let guard = write_mcp_config(&servers, "DROP-TEST", Path::new("/tmp")).unwrap();
        let path = guard.path.clone();
        assert!(path.exists());
        drop(guard);
        assert!(!path.exists());
    }
}
