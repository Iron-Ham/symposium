use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("config error: {0}")]
    Config(String),

    #[error("config parse error: {0}")]
    ConfigParse(String),

    #[error("workflow file error: {0}")]
    Workflow(String),

    #[error("tracker error: {0}")]
    Tracker(String),

    #[error("MCP protocol error: {0}")]
    Mcp(String),

    #[error("workspace error: {0}")]
    Workspace(String),

    #[error("agent error: {0}")]
    Agent(String),

    #[error("agent protocol error: {0}")]
    AgentProtocol(String),

    #[error("prompt render error: {0}")]
    Prompt(String),

    #[error("hook execution error: {0}")]
    Hook(String),

    #[error("orchestrator error: {0}")]
    Orchestrator(String),

    #[error("server error: {0}")]
    Server(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("yaml error: {0}")]
    Yaml(#[from] serde_yaml::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
