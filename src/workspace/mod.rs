pub mod hooks;
pub mod safety;

use crate::config::schema::ServiceConfig;
use crate::domain::issue::Issue;
use crate::error::{Error, Result};
use crate::prompt;
use std::path::PathBuf;
use tokio::sync::watch;

pub struct WorkspaceManager {
    config_rx: watch::Receiver<ServiceConfig>,
}

impl WorkspaceManager {
    pub fn new(config_rx: watch::Receiver<ServiceConfig>) -> Self {
        Self { config_rx }
    }

    fn workspace_root(&self) -> PathBuf {
        let config = self.config_rx.borrow();
        PathBuf::from(&config.workspace.root)
    }

    /// Get the workspace directory for a given issue key.
    pub fn workspace_dir(&self, issue_key: &str) -> Result<PathBuf> {
        let sanitized = safety::sanitize_key(issue_key);
        let root = self.workspace_root();
        let dir = root.join(&sanitized);
        safety::check_containment(&root, &dir)?;
        Ok(dir)
    }

    /// Render a hook script through Liquid with issue context.
    fn render_hook(&self, hook: &str, issue: &Issue, attempt: Option<u32>) -> Result<String> {
        prompt::build_prompt(hook, issue, attempt)
    }

    /// Ensure the workspace directory exists, run after_create hook if newly created.
    pub async fn ensure(&self, issue: &Issue) -> Result<PathBuf> {
        let dir = self.workspace_dir(&issue.identifier)?;
        let newly_created = !dir.exists();

        if newly_created {
            tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                Error::Workspace(format!("failed to create workspace {}: {e}", dir.display()))
            })?;
            tracing::info!(issue_key = issue.identifier, path = %dir.display(), "created workspace");

            let config = self.config_rx.borrow().clone();
            if let Some(hook) = &config.hooks.after_create {
                let rendered = self.render_hook(hook, issue, None)?;
                hooks::run_hook(&rendered, &dir, config.hooks.timeout()).await?;
            }
        }

        Ok(dir)
    }

    /// Run the before_run hook in the workspace.
    pub async fn prepare(&self, issue: &Issue, attempt: Option<u32>) -> Result<PathBuf> {
        let dir = self.workspace_dir(&issue.identifier)?;
        let config = self.config_rx.borrow().clone();
        if let Some(hook) = &config.hooks.before_run {
            let rendered = self.render_hook(hook, issue, attempt)?;
            hooks::run_hook(&rendered, &dir, config.hooks.timeout()).await?;
        }
        Ok(dir)
    }

    /// Run the after_run hook in the workspace.
    pub async fn finish(&self, issue: &Issue, success: bool) -> Result<()> {
        let dir = self.workspace_dir(&issue.identifier)?;
        let config = self.config_rx.borrow().clone();
        if let Some(hook) = &config.hooks.after_run {
            let rendered = self.render_hook(hook, issue, None)?;
            let mut env = std::collections::HashMap::new();
            env.insert(
                "RUN_SUCCESS".to_string(),
                if success { "true" } else { "false" }.to_string(),
            );
            hooks::run_hook_with_env(&rendered, &dir, config.hooks.timeout(), &env).await?;
        }
        Ok(())
    }

    /// Remove a workspace directory (for terminal issues).
    pub async fn remove(&self, issue_key: &str) -> Result<()> {
        let dir = self.workspace_dir(issue_key)?;
        if dir.exists() {
            tokio::fs::remove_dir_all(&dir).await.map_err(|e| {
                Error::Workspace(format!("failed to remove workspace {}: {e}", dir.display()))
            })?;
            tracing::info!(issue_key, path = %dir.display(), "removed workspace");
        }
        Ok(())
    }

    /// List all existing workspace directories.
    pub fn list_workspaces(&self) -> Result<Vec<String>> {
        let root = self.workspace_root();
        if !root.exists() {
            return Ok(vec![]);
        }
        let mut keys = Vec::new();
        for entry in std::fs::read_dir(&root).map_err(|e| {
            Error::Workspace(format!("failed to read workspace root {}: {e}", root.display()))
        })? {
            let entry = entry.map_err(|e| Error::Workspace(format!("readdir error: {e}")))?;
            if entry.path().is_dir()
                && let Some(name) = entry.file_name().to_str() {
                    keys.push(name.to_string());
                }
        }
        Ok(keys)
    }
}
