use serde::Deserialize;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct ServiceConfig {
    pub tracker: TrackerConfig,
    pub polling: PollingConfig,
    pub workspace: WorkspaceConfig,
    pub hooks: HooksConfig,
    pub agent: AgentConfig,
    pub codex: CodexConfig,
    pub server: ServerConfig,
    pub prompt_template: String,
}


#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct TrackerConfig {
    pub kind: String,
    pub mcp_command: String,
    pub mcp_url: Option<String>,
    pub database_id: String,
    pub active_states: Vec<String>,
    pub terminal_states: Vec<String>,
    pub property_id: String,
    pub property_title: String,
    pub property_status: String,
    pub property_priority: String,
    pub property_description: String,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            kind: "notion".to_string(),
            mcp_command: "npx -y @notionhq/notion-mcp-server".to_string(),
            mcp_url: None,
            database_id: String::new(),
            active_states: vec!["Todo".to_string(), "In Progress".to_string()],
            terminal_states: vec!["Done".to_string(), "Cancelled".to_string()],
            property_id: "ID".to_string(),
            property_title: "Name".to_string(),
            property_status: "Status".to_string(),
            property_priority: "Priority".to_string(),
            property_description: "Description".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct PollingConfig {
    pub interval_ms: u64,
}

impl Default for PollingConfig {
    fn default() -> Self {
        Self { interval_ms: 30000 }
    }
}

impl PollingConfig {
    pub fn interval(&self) -> Duration {
        Duration::from_millis(self.interval_ms)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct WorkspaceConfig {
    pub root: String,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            root: "~/symposium_workspaces".to_string(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    pub after_create: Option<String>,
    pub before_run: Option<String>,
    pub after_run: Option<String>,
    pub timeout_ms: u64,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            after_create: None,
            before_run: None,
            after_run: None,
            timeout_ms: 300_000,
        }
    }
}

impl HooksConfig {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.timeout_ms)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub max_concurrent_agents: usize,
    pub max_turns: u32,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            max_concurrent_agents: 5,
            max_turns: 20,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CodexConfig {
    pub command: String,
    pub turn_timeout_ms: u64,
    pub stall_timeout_ms: u64,
}

impl Default for CodexConfig {
    fn default() -> Self {
        Self {
            command: "claude-code app-server".to_string(),
            turn_timeout_ms: 3_600_000,
            stall_timeout_ms: 300_000,
        }
    }
}

impl CodexConfig {
    pub fn turn_timeout(&self) -> Duration {
        Duration::from_millis(self.turn_timeout_ms)
    }

    pub fn stall_timeout(&self) -> Duration {
        Duration::from_millis(self.stall_timeout_ms)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub port: u16,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self { port: 8080 }
    }
}
