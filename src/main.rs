use clap::{Parser, Subcommand};
use std::collections::HashSet;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "symposium", about = "Symphony spec orchestrator for Notion + coding agents")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,

    /// Paths to WORKFLOW.md files (one per workflow)
    #[arg(default_value = "WORKFLOW.md")]
    workflow_paths: Vec<PathBuf>,

    /// Global maximum concurrent agents across all workflows
    #[arg(long)]
    max_agents: Option<usize>,

    /// HTTP server port (overrides config; defaults to config value or 8080)
    #[arg(long)]
    port: Option<u16>,

    /// Output logs as JSON
    #[arg(long)]
    json_logs: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Re-authorize an OAuth-protected MCP server. Purges any cached
    /// tokens and runs the browser flow. Use this when the refresh
    /// token has expired (e.g. after a long idle period).
    Auth {
        /// MCP server URL (e.g. https://mcp.sentry.dev/mcp)
        url: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    symposium::logging::init(cli.json_logs);

    if let Some(Command::Auth { url }) = cli.command {
        let mut oauth = symposium::tracker::oauth::OAuthClient::new(&url);
        oauth.reauthorize().await?;
        println!("Authorized {url}. Cached tokens saved. Restart the orchestrator to pick them up.");
        return Ok(());
    }

    // Canonicalize workflow paths
    let workflow_paths: Vec<PathBuf> = cli
        .workflow_paths
        .iter()
        .map(std::fs::canonicalize)
        .collect::<Result<Vec<_>, _>>()?;

    // Build per-workflow handles and watchers
    let mut workflows = Vec::new();
    let mut watchers = Vec::new();
    let mut seen_ids = HashSet::new();
    let mut first_port = None;

    for path in &workflow_paths {
        let config = symposium::config::workflow::parse_workflow_file(path)?;
        let wf_id = symposium::domain::workflow::WorkflowId::from_path(path);

        if !seen_ids.insert(wf_id.0.clone()) {
            anyhow::bail!(
                "duplicate workflow ID \"{}\": each WORKFLOW file must have a unique name",
                wf_id
            );
        }

        tracing::info!(workflow = %wf_id, path = %path.display(), "loaded workflow config");

        // Capture port from first workflow config (unless CLI overrides)
        if first_port.is_none() {
            first_port = Some(config.server.port);
        }

        let (config_tx, config_rx) = tokio::sync::watch::channel(config);

        let watcher = symposium::config::watch::spawn_watcher(path.clone(), config_tx)?;
        watchers.push(watcher);

        workflows.push(symposium::domain::workflow::WorkflowHandle {
            id: wf_id,
            config_rx,
        });
    }

    // CLI --port overrides config; config overrides default 8080
    let port = cli.port.unwrap_or(first_port.unwrap_or(8080));
    tracing::info!(
        workflows = workflows.len(),
        port,
        max_agents = ?cli.max_agents,
        "starting symposium"
    );

    // Build shared orchestrator state with persistence for tracked PRs
    let state_dir = std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join(".symposium");
    let state = symposium::domain::state::OrchestratorState::with_persistence(state_dir);

    // Discover open PRs from existing workspaces (covers restarts before
    // persistence was added, or if the state file was lost)
    symposium::orchestrator::reconcile::discover_open_prs(&state, &workflows).await;

    // Build orchestrator and get event channel for server
    let mut orchestrator =
        symposium::orchestrator::Orchestrator::new(state.clone(), workflows, cli.max_agents);
    let event_tx = orchestrator.event_sender();

    // Start HTTP server in background
    let server_state = state.clone();
    let server_handle = tokio::spawn(async move {
        if let Err(e) = symposium::server::run(server_state, port, Some(event_tx)).await {
            tracing::error!("server error: {e}");
        }
    });

    // Run orchestrator event loop
    tokio::select! {
        result = orchestrator.run() => {
            if let Err(e) = result {
                tracing::error!("orchestrator error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("shutting down");
        }
        result = server_handle => {
            if let Err(e) = result {
                tracing::error!("server task panicked: {e}");
            }
        }
    }

    // Keep watchers alive until shutdown
    drop(watchers);

    Ok(())
}
