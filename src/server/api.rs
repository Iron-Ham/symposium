use crate::domain::state::OrchestratorState;
use crate::orchestrator::OrchestratorEvent;
use axum::{
    Router,
    extract::{Path, State},
    http::StatusCode,
    response::Json,
    routing::{get, post},
};
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub struct AppState {
    pub orchestrator: OrchestratorState,
    pub event_tx: Option<mpsc::Sender<OrchestratorEvent>>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(dashboard))
        .route("/issue/{*id}", get(issue_detail))
        .route("/api/v1/state", get(get_state))
        .route("/api/v1/issues/{*id}", get(get_issue))
        .route("/api/v1/refresh", post(refresh))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn dashboard(State(state): State<AppState>) -> axum::response::Html<String> {
    let snapshot = state.orchestrator.snapshot();
    let html = super::dashboard::render(&snapshot);
    axum::response::Html(html)
}

async fn issue_detail(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> axum::response::Html<String> {
    let snapshot = state.orchestrator.snapshot();
    let html = super::dashboard::render_issue_detail(&snapshot, &id);
    axum::response::Html(html)
}

async fn get_state(State(state): State<AppState>) -> Json<serde_json::Value> {
    let snapshot = state.orchestrator.snapshot();
    Json(serde_json::to_value(snapshot).unwrap_or_default())
}

async fn get_issue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, StatusCode> {
    match state.orchestrator.get_issue_detail(&id) {
        Some(entry) => Ok(Json(serde_json::to_value(entry).unwrap_or_default())),
        None => Err(StatusCode::NOT_FOUND),
    }
}

async fn refresh(State(state): State<AppState>) -> StatusCode {
    if let Some(ref tx) = state.event_tx {
        match tx.send(OrchestratorEvent::RefreshRequested).await {
            Ok(()) => {
                tracing::info!("refresh requested via API");
                StatusCode::OK
            }
            Err(e) => {
                tracing::error!("failed to send refresh event: {e}");
                StatusCode::INTERNAL_SERVER_ERROR
            }
        }
    } else {
        tracing::info!("refresh requested but no event channel");
        StatusCode::OK
    }
}
