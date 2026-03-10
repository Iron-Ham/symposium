use crate::config::schema::ServiceConfig;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::Path;
use tokio::sync::watch;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkflowId(pub String);

impl WorkflowId {
    /// Derive a workflow ID from a file path.
    ///
    /// `WORKFLOW.md` → `"default"`
    /// `WORKFLOW.bugs.md` → `"bugs"`
    /// `WORKFLOW.sentry.md` → `"sentry"`
    /// `path/to/WORKFLOW.foo.bar.md` → `"foo.bar"`
    pub fn from_path(path: &Path) -> Self {
        let stem = path
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("WORKFLOW.md");

        // Strip "WORKFLOW." prefix and ".md" suffix
        let without_ext = stem.strip_suffix(".md").unwrap_or(stem);
        let id = without_ext
            .strip_prefix("WORKFLOW.")
            .unwrap_or(without_ext);

        if id.is_empty() || id == "WORKFLOW" || without_ext == "WORKFLOW" {
            Self("default".to_string())
        } else {
            Self(id.to_string())
        }
    }

    /// Build a composite state key: `"{workflow_id}/{issue_id}"`.
    ///
    /// For the "default" workflow, this still prefixes so that all workflows
    /// use a consistent key format.
    pub fn state_key(&self, issue_id: &str) -> String {
        format!("{}/{}", self.0, issue_id)
    }
}

impl fmt::Display for WorkflowId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A handle to a single workflow — its identity plus a live config feed.
///
/// Cloning is cheap: `WorkflowId` is a small `String` and `watch::Receiver`
/// is backed by an `Arc`, so clone is just a reference-count bump.
#[derive(Clone)]
pub struct WorkflowHandle {
    pub id: WorkflowId,
    pub config_rx: watch::Receiver<ServiceConfig>,
}

impl fmt::Debug for WorkflowHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkflowHandle")
            .field("id", &self.id)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn from_path_default() {
        assert_eq!(
            WorkflowId::from_path(&PathBuf::from("WORKFLOW.md")),
            WorkflowId("default".to_string())
        );
    }

    #[test]
    fn from_path_named() {
        assert_eq!(
            WorkflowId::from_path(&PathBuf::from("WORKFLOW.bugs.md")),
            WorkflowId("bugs".to_string())
        );
    }

    #[test]
    fn from_path_nested() {
        assert_eq!(
            WorkflowId::from_path(&PathBuf::from("/home/user/project/WORKFLOW.sentry.md")),
            WorkflowId("sentry".to_string())
        );
    }

    #[test]
    fn from_path_multi_dot() {
        assert_eq!(
            WorkflowId::from_path(&PathBuf::from("WORKFLOW.foo.bar.md")),
            WorkflowId("foo.bar".to_string())
        );
    }

    #[test]
    fn state_key_format() {
        let wf = WorkflowId("bugs".to_string());
        assert_eq!(wf.state_key("TASK-123"), "bugs/TASK-123");
    }

    #[test]
    fn state_key_default() {
        let wf = WorkflowId("default".to_string());
        assert_eq!(wf.state_key("TASK-456"), "default/TASK-456");
    }
}
