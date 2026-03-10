use crate::error::{Error, Result};
use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

/// Run a shell hook script in the given working directory with a timeout.
pub async fn run_hook(script: &str, cwd: &Path, timeout: Duration) -> Result<()> {
    run_hook_with_env(script, cwd, timeout, &HashMap::new()).await
}

/// Run a shell hook and return its stdout output on success.
pub async fn run_hook_with_output(
    script: &str,
    cwd: &Path,
    timeout: Duration,
) -> Result<String> {
    let mut cmd = Command::new("sh");
    cmd.arg("-c").arg(script).current_dir(cwd);
    cmd.stdout(std::process::Stdio::piped());
    cmd.stderr(std::process::Stdio::piped());

    let child = cmd.spawn().map_err(|e| {
        Error::Hook(format!("failed to spawn hook: {e}"))
    })?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.is_empty() {
                tracing::debug!(stderr = %stderr.trim(), "hook stderr");
            }
            if output.status.success() {
                Ok(String::from_utf8_lossy(&output.stdout).to_string())
            } else {
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stdout.is_empty() {
                    tracing::debug!(stdout = %stdout.trim(), "hook stdout");
                }
                Err(Error::Hook(format!("hook exited with {}", output.status)))
            }
        }
        Ok(Err(e)) => Err(Error::Hook(format!("hook I/O error: {e}"))),
        Err(_) => {
            Err(Error::Hook(format!(
                "hook timed out after {}s",
                timeout.as_secs()
            )))
        }
    }
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

    let child = cmd.spawn().map_err(|e| {
        Error::Hook(format!("failed to spawn hook: {e}"))
    })?;

    match tokio::time::timeout(timeout, child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if output.status.success() {
                Ok(())
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let stdout = String::from_utf8_lossy(&output.stdout);
                if !stderr.is_empty() {
                    tracing::warn!(stderr = %stderr.trim(), "hook stderr");
                }
                if !stdout.is_empty() {
                    tracing::debug!(stdout = %stdout.trim(), "hook stdout");
                }
                Err(Error::Hook(format!("hook exited with {}", output.status)))
            }
        }
        Ok(Err(e)) => Err(Error::Hook(format!("hook I/O error: {e}"))),
        Err(_) => {
            // child was consumed by wait_with_output — process already finished or will be reaped
            Err(Error::Hook(format!(
                "hook timed out after {}s",
                timeout.as_secs()
            )))
        }
    }
}
