pub mod dispatch;
pub mod pr_review;
pub mod reconcile;
pub mod retry;
pub mod tick;

use crate::domain::state::OrchestratorState;
use crate::domain::workflow::WorkflowHandle;
use crate::error::Result;

/// Events that drive the orchestrator.
#[derive(Debug)]
pub enum OrchestratorEvent {
    WorkerCompleted {
        state_key: String,
        success: bool,
        error: Option<String>,
    },
    RetryFired {
        state_key: String,
        workflow_id: String,
    },
    AgentUpdate {
        state_key: String,
        event: serde_json::Value,
    },
    ConfigReloaded {
        workflow_id: String,
    },
    RefreshRequested,
}

pub struct Orchestrator {
    state: OrchestratorState,
    workflows: Vec<WorkflowHandle>,
    global_max_agents: Option<usize>,
    event_tx: tokio::sync::mpsc::Sender<OrchestratorEvent>,
    event_rx: tokio::sync::mpsc::Receiver<OrchestratorEvent>,
}

impl Orchestrator {
    pub fn new(
        state: OrchestratorState,
        workflows: Vec<WorkflowHandle>,
        global_max_agents: Option<usize>,
    ) -> Self {
        let (event_tx, event_rx) = tokio::sync::mpsc::channel(256);
        Self {
            state,
            workflows,
            global_max_agents,
            event_tx,
            event_rx,
        }
    }

    pub fn event_sender(&self) -> tokio::sync::mpsc::Sender<OrchestratorEvent> {
        self.event_tx.clone()
    }

    pub async fn run(&mut self) -> Result<()> {
        // Use the minimum polling interval across all workflows as the base timer
        let base_interval = self
            .workflows
            .iter()
            .map(|wf| wf.config_rx.borrow().polling.interval())
            .min()
            .unwrap_or(std::time::Duration::from_secs(30));

        let mut tick_interval = tokio::time::interval(base_interval);

        // Track per-workflow last tick times
        let mut last_tick: std::collections::HashMap<String, tokio::time::Instant> =
            std::collections::HashMap::new();

        tracing::info!(
            workflows = self.workflows.len(),
            base_interval_ms = base_interval.as_millis() as u64,
            "orchestrator running"
        );

        // Spawn config reload forwarding tasks
        for wf in &self.workflows {
            let mut rx = wf.config_rx.clone();
            let tx = self.event_tx.clone();
            let wf_id = wf.id.0.clone();
            tokio::spawn(async move {
                while rx.changed().await.is_ok() {
                    let _ = tx
                        .send(OrchestratorEvent::ConfigReloaded {
                            workflow_id: wf_id.clone(),
                        })
                        .await;
                }
            });
        }

        loop {
            tokio::select! {
                _ = tick_interval.tick() => {
                    if let Err(e) = self.on_tick(&mut last_tick).await {
                        tracing::error!("tick error: {e}");
                    }
                }
                Some(event) = self.event_rx.recv() => {
                    self.on_event(event, &mut last_tick, &mut tick_interval).await;
                }
            }
        }
    }

    async fn on_tick(
        &mut self,
        last_tick: &mut std::collections::HashMap<String, tokio::time::Instant>,
    ) -> Result<()> {
        // 1. Reconcile: check for stalled workers (reads per-entry stall_timeout)
        reconcile::check_stalled(&self.state);

        let now = tokio::time::Instant::now();

        // Collect workflows that are due for a tick
        let due_workflows: Vec<_> = self
            .workflows
            .iter()
            .filter(|wf| {
                let config = wf.config_rx.borrow().clone();
                let interval = config.polling.interval();
                match last_tick.get(&wf.id.0) {
                    Some(last) => now.duration_since(*last) >= interval,
                    None => true,
                }
            })
            .cloned()
            .collect();

        for wf in &due_workflows {
            last_tick.insert(wf.id.0.clone(), now);
        }

        if self.global_max_agents.is_some() {
            // 2a. Sequential: when a global cap is set, run ticks sequentially so
            //     that start_session() (which increments running_count) is visible
            //     to the next workflow's eligibility check. Without this, parallel
            //     ticks can both observe free capacity and dispatch simultaneously,
            //     exceeding the global limit.
            for wf in &due_workflows {
                let wf_id = wf.id.0.clone();
                let result =
                    tick::run_workflow_tick(wf, &self.state, &self.event_tx, self.global_max_agents)
                        .await;
                if let Err(e) = result {
                    tracing::error!(workflow = %wf_id, "tick error: {e}");
                }
            }
        } else {
            // 2b. Parallel: no global cap, workflows are independent. Each tick
            //     involves MCP I/O (connecting to Notion/Sentry), so running them
            //     in parallel avoids N × connection_time sequential latency.
            let mut set = tokio::task::JoinSet::new();

            for wf in &due_workflows {
                let handle = wf.clone();
                let state = self.state.clone();
                let event_tx = self.event_tx.clone();

                set.spawn(async move {
                    let wf_id = handle.id.0.clone();
                    let result =
                        tick::run_workflow_tick(&handle, &state, &event_tx, None).await;
                    (wf_id, result)
                });
            }

            // 3. Collect results from all concurrent ticks
            while let Some(join_result) = set.join_next().await {
                match join_result {
                    Ok((wf_id, Err(e))) => {
                        tracing::error!(workflow = %wf_id, "tick error: {e}");
                    }
                    Err(join_err) => {
                        tracing::error!("workflow tick task panicked: {join_err}");
                    }
                    Ok((_, Ok(()))) => {}
                }
            }
        }

        Ok(())
    }

    async fn on_event(
        &mut self,
        event: OrchestratorEvent,
        last_tick: &mut std::collections::HashMap<String, tokio::time::Instant>,
        tick_interval: &mut tokio::time::Interval,
    ) {
        match event {
            OrchestratorEvent::WorkerCompleted {
                state_key,
                success,
                error,
            } => {
                tracing::info!(state_key, success, ?error, "worker completed");
                self.state.mark_worker_done(&state_key, success, error);
            }
            OrchestratorEvent::RetryFired {
                state_key,
                workflow_id,
            } => {
                tracing::info!(state_key, workflow_id, "retry timer fired");
                self.state.mark_retry_ready(&state_key);
            }
            OrchestratorEvent::AgentUpdate { state_key, event: _ } => {
                tracing::debug!(state_key, "agent update");
            }
            OrchestratorEvent::ConfigReloaded { workflow_id } => {
                tracing::info!(workflow_id, "config reloaded");
                let new_base = self
                    .workflows
                    .iter()
                    .map(|wf| wf.config_rx.borrow().polling.interval())
                    .min()
                    .unwrap_or(std::time::Duration::from_secs(30));
                tracing::info!(new_base_ms = new_base.as_millis() as u64, "rebuilding tick timer");
                *tick_interval = tokio::time::interval(new_base);
            }
            OrchestratorEvent::RefreshRequested => {
                tracing::info!("manual refresh requested");
                if let Err(e) = self.on_tick(last_tick).await {
                    tracing::error!("refresh tick error: {e}");
                }
            }
        }
    }
}
