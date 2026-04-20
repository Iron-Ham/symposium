pub mod hooks;
pub mod safety;

use crate::config::schema::ServiceConfig;
use crate::domain::issue::Issue;
use crate::error::{Error, Result};
use crate::prompt;
use std::path::{Path, PathBuf};
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

    /// Render a hook script through Liquid with issue context and workspace path.
    fn render_hook(
        &self,
        hook: &str,
        issue: &Issue,
        attempt: Option<u32>,
        workspace: &Path,
    ) -> Result<String> {
        prompt::build_prompt_with_workspace(
            hook,
            issue,
            attempt,
            workspace.to_str(),
        )
    }

    /// Ensure the workspace directory exists, run after_create hook if newly created.
    pub async fn ensure(&self, issue: &Issue) -> Result<PathBuf> {
        let dir = self.workspace_dir(&issue.identifier)?;
        let newly_created = !dir.exists();

        if newly_created {
            let config = self.config_rx.borrow().clone();
            if let Some(hook) = &config.hooks.after_create {
                // Ensure parent dir exists; the hook itself creates the workspace
                // (e.g. git worktree add creates the target directory)
                if let Some(parent) = dir.parent() {
                    tokio::fs::create_dir_all(parent).await.map_err(|e| {
                        Error::Workspace(format!(
                            "failed to create workspace parent {}: {e}",
                            parent.display()
                        ))
                    })?;
                }
                tracing::info!(issue_key = issue.identifier, path = %dir.display(), "creating workspace");
                let rendered = self.render_hook(hook, issue, None, &dir)?;
                // Run hook from the parent directory since workspace doesn't exist yet
                let cwd = dir.parent().unwrap_or(&dir);
                hooks::run_hook(&rendered, cwd, config.hooks.timeout()).await?;
            } else {
                tokio::fs::create_dir_all(&dir).await.map_err(|e| {
                    Error::Workspace(format!(
                        "failed to create workspace {}: {e}",
                        dir.display()
                    ))
                })?;
                tracing::info!(issue_key = issue.identifier, path = %dir.display(), "created workspace");
            }
        }

        Ok(dir)
    }

    /// Run the before_run hook in the workspace.
    pub async fn prepare(&self, issue: &Issue, attempt: Option<u32>) -> Result<PathBuf> {
        let dir = self.workspace_dir(&issue.identifier)?;
        let config = self.config_rx.borrow().clone();
        if let Some(hook) = &config.hooks.before_run {
            let rendered = self.render_hook(hook, issue, attempt, &dir)?;
            hooks::run_hook(&rendered, &dir, config.hooks.timeout()).await?;
        }
        Ok(dir)
    }

    /// Run the after_run hook in the workspace.
    pub async fn finish(&self, issue: &Issue, success: bool) -> Result<()> {
        let dir = self.workspace_dir(&issue.identifier)?;
        let config = self.config_rx.borrow().clone();
        if let Some(hook) = &config.hooks.after_run {
            let rendered = self.render_hook(hook, issue, None, &dir)?;
            let mut env = std::collections::HashMap::new();
            env.insert(
                "RUN_SUCCESS".to_string(),
                if success { "true" } else { "false" }.to_string(),
            );
            hooks::run_hook_with_env(&rendered, &dir, config.hooks.timeout(), &env).await?;
        }
        Ok(())
    }

    /// Remove a workspace directory (for terminal issues or reaping).
    ///
    /// If `hooks.before_remove` is configured, it is rendered with the issue
    /// (if available) and the workspace path, then executed from the workspace
    /// root's parent directory — this matters for `git worktree remove`, which
    /// refuses to run from inside the target worktree. A final `remove_dir_all`
    /// sweeps anything the hook left behind so disk is always reclaimed.
    pub async fn remove(&self, issue_key: &str) -> Result<()> {
        self.remove_with_issue(issue_key, None).await
    }

    /// Same as [`remove`] but passes an Issue into the hook template so users
    /// can reference `{{ issue.identifier }}` etc. from `before_remove`.
    pub async fn remove_with_issue(&self, issue_key: &str, issue: Option<&Issue>) -> Result<()> {
        let dir = self.workspace_dir(issue_key)?;
        if !dir.exists() {
            return Ok(());
        }

        let config = self.config_rx.borrow().clone();
        if let Some(hook) = &config.hooks.before_remove {
            // Synthesize a minimal issue if one wasn't supplied (age-based
            // reaper path doesn't have tracker metadata for orphan workspaces).
            let synth;
            let issue_ref = match issue {
                Some(i) => i,
                None => {
                    synth = Issue {
                        identifier: issue_key.to_string(),
                        title: String::new(),
                        description: None,
                        status: String::new(),
                        priority: None,
                        url: None,
                        notion_page_id: None,
                        blockers: vec![],
                        source: "reaper".to_string(),
                        extra: std::collections::HashMap::new(),
                        comments: vec![],
                        workflow_id: String::new(),
                    };
                    &synth
                }
            };
            let rendered = self.render_hook(hook, issue_ref, None, &dir)?;
            let cwd = dir.parent().unwrap_or(&dir);
            if let Err(e) = hooks::run_hook(&rendered, cwd, config.hooks.timeout()).await {
                tracing::warn!(
                    issue_key,
                    path = %dir.display(),
                    "before_remove hook failed, falling back to filesystem removal: {e}"
                );
            }
        }

        if dir.exists() {
            tokio::fs::remove_dir_all(&dir).await.map_err(|e| {
                Error::Workspace(format!("failed to remove workspace {}: {e}", dir.display()))
            })?;
        }
        tracing::info!(issue_key, path = %dir.display(), "removed workspace");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::ServiceConfig;
    use tempfile::TempDir;

    fn manager_with_root(root: &Path) -> (WorkspaceManager, watch::Sender<ServiceConfig>) {
        let mut config = ServiceConfig::default();
        config.workspace.root = root.to_string_lossy().into_owned();
        let (tx, rx) = watch::channel(config);
        (WorkspaceManager::new(rx), tx)
    }

    #[tokio::test]
    async fn remove_deletes_directory() {
        let tmp = TempDir::new().unwrap();
        let (ws, _tx) = manager_with_root(tmp.path());
        let target = tmp.path().join("ABC-1");
        tokio::fs::create_dir_all(&target).await.unwrap();
        tokio::fs::write(target.join("file.txt"), b"hi").await.unwrap();

        ws.remove("ABC-1").await.unwrap();
        assert!(!target.exists(), "workspace dir should be gone");
    }

    #[tokio::test]
    async fn remove_runs_before_remove_hook() {
        let tmp = TempDir::new().unwrap();
        let sentinel = tmp.path().join("hook-ran");
        let (ws, tx) = manager_with_root(tmp.path());

        // Hook writes a sentinel file with the workspace path so we can
        // verify it ran with the expected template expansion.
        let hook = format!(
            r#"printf '%s' "{{{{ workspace }}}}" > {}"#,
            sentinel.display()
        );
        tx.send_modify(|c| c.hooks.before_remove = Some(hook));

        let target = tmp.path().join("XYZ-9");
        tokio::fs::create_dir_all(&target).await.unwrap();

        ws.remove("XYZ-9").await.unwrap();

        let written = tokio::fs::read_to_string(&sentinel).await.unwrap();
        assert_eq!(written, target.to_string_lossy());
        assert!(!target.exists(), "workspace dir should still be gone after hook");
    }

    #[tokio::test]
    async fn remove_falls_back_when_hook_fails() {
        let tmp = TempDir::new().unwrap();
        let (ws, tx) = manager_with_root(tmp.path());
        tx.send_modify(|c| c.hooks.before_remove = Some("exit 1".into()));

        let target = tmp.path().join("FAIL-1");
        tokio::fs::create_dir_all(&target).await.unwrap();
        ws.remove("FAIL-1").await.unwrap();

        assert!(!target.exists(), "fallback remove_dir_all should still reclaim disk");
    }

    #[tokio::test]
    async fn remove_missing_workspace_is_noop() {
        let tmp = TempDir::new().unwrap();
        let (ws, _tx) = manager_with_root(tmp.path());
        ws.remove("NEVER-EXISTED").await.unwrap();
    }
}
