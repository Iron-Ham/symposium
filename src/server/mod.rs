pub mod api;
pub mod dashboard;

use crate::domain::state::OrchestratorState;
use crate::error::{Error, Result};
use crate::orchestrator::OrchestratorEvent;
use tokio::sync::mpsc;

pub async fn run(
    state: OrchestratorState,
    port: u16,
    event_tx: Option<mpsc::Sender<OrchestratorEvent>>,
) -> Result<()> {
    let app_state = api::AppState {
        orchestrator: state,
        event_tx,
    };
    let app = api::router(app_state);
    let listener = tokio::net::TcpListener::bind(format!("0.0.0.0:{port}"))
        .await
        .map_err(|e| Error::Server(format!("failed to bind port {port}: {e}")))?;

    tracing::info!(port, "HTTP server listening");
    axum::serve(listener, app)
        .await
        .map_err(|e| Error::Server(format!("server error: {e}")))?;

    Ok(())
}
