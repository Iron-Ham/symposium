use super::schema::ServiceConfig;
use super::workflow;
use crate::error::{Error, Result};
use notify_debouncer_mini::{new_debouncer, DebouncedEvent, DebouncedEventKind};
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;

/// Spawn a file watcher that re-parses a WORKFLOW.md on changes and sends
/// updated configs through the watch channel.
///
/// Returns the debouncer handle — it must be kept alive for watching to continue.
pub fn spawn_watcher(
    path: PathBuf,
    tx: watch::Sender<ServiceConfig>,
) -> Result<impl Drop> {
    let watch_path = path.clone();

    let mut debouncer = new_debouncer(Duration::from_millis(500), move |res| {
        let events: Vec<DebouncedEvent> = match res {
            Ok(events) => events,
            Err(e) => {
                tracing::error!("file watch error: {:?}", e);
                return;
            }
        };

        let dominated = events
            .iter()
            .any(|e| e.kind == DebouncedEventKind::Any);

        if !dominated {
            return;
        }

        match workflow::parse_workflow_file(&watch_path) {
            Ok(config) => {
                tracing::info!("reloaded config from {}", watch_path.display());
                let _ = tx.send(config);
            }
            Err(e) => {
                tracing::error!(
                    "failed to parse {}: {}, keeping previous config",
                    watch_path.display(),
                    e
                );
            }
        }
    })
    .map_err(|e| Error::Config(format!("failed to create file watcher: {}", e)))?;

    let parent = path
        .parent()
        .ok_or_else(|| Error::Config("workflow path has no parent directory".to_string()))?;

    debouncer
        .watcher()
        .watch(parent, notify::RecursiveMode::NonRecursive)
        .map_err(|e| Error::Config(format!("failed to watch {}: {}", parent.display(), e)))?;

    Ok(debouncer)
}
