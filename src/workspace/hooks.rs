use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

/// Run a shell hook script in the given working directory with a timeout.
pub async fn run_hook(script: &str, cwd: &Path, timeout: Duration) -> Result<()> {
    run_hook_with_env(script, cwd, timeout, &HashMap::new()).await
}

/// Run a shell hook with extra environment variables.
pub async fn run_hook_with_env(
    script: &str,
    cwd: &Path,
    timeout: Duration,
    env: &HashMap<String, String>,
) -> Result<()> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).current_dir(cwd);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let mut child = cmd.spawn().map_err(|e| {
        Error::Hook(format!("failed to spawn hook: {e}"))
    })?;

    match tokio::time::timeout(timeout, child.wait()).await {
        Ok(Ok(status)) => {
            if status.success() {
                Ok(())
            } else {
                Err(Error::Hook(format!("hook exited with {status}")))
            }
        }
        Ok(Err(e)) => Err(Error::Hook(format!("hook I/O error: {e}"))),
        Err(_) => {
            let _ = child.start_kill();
            Err(Error::Hook(format!(
                "hook timed out after {}s",
                timeout.as_secs()
            )))
        }
    }
}
