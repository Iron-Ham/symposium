use clap::Parser;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "symposium", about = "Symphony spec orchestrator for Notion + coding agents")]
struct Cli {
    /// Path to the WORKFLOW.md file
    #[arg(default_value = "WORKFLOW.md")]
    workflow_path: PathBuf,

    /// HTTP server port (overrides config; defaults to config value or 8080)
    #[arg(long)]
    port: Option<u16>,

    /// Output logs as JSON
    #[arg(long)]
    json_logs: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    symposium::logging::init(cli.json_logs);

    // Parse config
    let config = symposium::config::workflow::parse_workflow_file(&cli.workflow_path)?;
    tracing::info!("loaded config from {}", cli.workflow_path.display());

    // CLI --port overrides config; config overrides default 8080
    let port = cli.port.unwrap_or(config.server.port);
    tracing::info!(workflow = %cli.workflow_path.display(), port, "starting symposium");

    // Create config watch channel
    let (config_tx, config_rx) = tokio::sync::watch::channel(config);

    // Start config file watcher
    let watch_path = cli.workflow_path.clone();
    let _watcher = symposium::config::watch::spawn_watcher(watch_path, config_tx)?;

    // Build shared orchestrator state
    let state = symposium::domain::state::OrchestratorState::new(config_rx.clone());

    // Build orchestrator and get event channel for server
    let mut orchestrator = symposium::orchestrator::Orchestrator::new(state.clone(), config_rx);
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

    Ok(())
}
