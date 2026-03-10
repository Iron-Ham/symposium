pub mod dispatch;
pub mod pr_review;
pub mod reconcile;
pub mod retry;
pub mod tick;

use crate::config::schema::ServiceConfig;
use crate::domain::state::OrchestratorState;
use crate::error::Result;
use tokio::sync::watch;

/// Events that drive the orchestrator.
#[derive(Debug)]
pub enum OrchestratorEvent {
    WorkerCompleted {
        issue_id: String,
        success: bool,
        error: Option<String>,
    },
    RetryFired {
        issue_id: String,
    },
    AgentUpdate {
        issue_id: String,
        event: serde_json::Value,
    },
    ConfigReloaded,
    RefreshRequested,
}

pub struct Orchestrator {
    state: OrchestratorState,
    config_rx: watch::Receiver<ServiceConfig>,
    event_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
    event_rx: tokio::sync::mpsc::Receiver<OrchestratorEvent>,
}

impl Orchestrator {
    pub fn new(state: OrchestratorState, config_rx: watch::Receiver<ServiceConfig>) -> Self {
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(256);
        Self {
            state,
            config_rx,
            event_tx,
            event_rx,
        }
    }

    pub fn event_sender(&self) -> tokio::sync::mpsc::Sender<OrchestratorEvent> {
        self.event_tx.clone()
    }

    pub async fn run(&mut self) -> Result<()> {
        let poll_interval = {
            let config = self.config_rx.borrow();
            config.polling.interval()
        };
        let mut tick_interval = tokio::time::interval(poll_interval);

        tracing::info!("orchestrator running, poll interval: {poll_interval:?}");

        loop {
            tokio::select! {
                _ = tick_interval.tick() => {
                    if let Err(e) = self.on_tick().await {
                        tracing::error!("tick error: {e}");
                    }
                }
                Some(event) = self.event_rx.recv() => {
                    self.on_event(event).await;
                }
                Ok(()) = self.config_rx.changed() => {
                    let config = self.config_rx.borrow();
                    let new_interval = config.polling.interval();
                    tick_interval = tokio::time::interval(new_interval);
                    tracing::info!("config reloaded, new poll interval: {new_interval:?}");
                }
            }
        }
    }

    async fn on_tick(&mut self) -> Result<()> {
        tick::run_tick(&self.state, &self.config_rx, &self.event_tx).await
    }

    async fn on_event(&mut self, event: OrchestratorEvent) {
        match event {
            OrchestratorEvent::WorkerCompleted {
                issue_id,
                success,
                error,
            } => {
                tracing::info!(issue_id, success, ?error, "worker completed");
                self.state.mark_worker_done(&issue_id, success, error);
            }
            OrchestratorEvent::RetryFired { issue_id } => {
                tracing::info!(issue_id, "retry timer fired");
                self.state.mark_retry_ready(&issue_id);
            }
            OrchestratorEvent::AgentUpdate { issue_id, event } => {
                tracing::debug!(issue_id, "agent update");
                // Events are now pushed directly from workers via state.push_agent_event
                let _ = (issue_id, event);
            }
            OrchestratorEvent::ConfigReloaded => {
                tracing::info!("config reloaded event");
            }
            OrchestratorEvent::RefreshRequested => {
                tracing::info!("manual refresh requested");
                if let Err(e) = self.on_tick().await {
                    tracing::error!("refresh tick error: {e}");
                }
            }
        }
    }
}
