use crate::error::{Error, Result};
use serde_json::Value;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout};

pub struct AgentProcess {
    child: Child,
    writer: Option<BufWriter<ChildStdin>>,
    reader: BufReader<ChildStdout>,
}

impl AgentProcess {
    /// Spawn claude in streaming JSON mode for multi-turn conversation.
    pub async fn spawn(command: &str, cwd: &Path) -> Result<Self> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        let (program, args) = parts
            .split_first()
            .ok_or_else(|| Error::Agent("empty agent command".into()))?;

        let mut cmd = tokio::process::Command::new(program);
        cmd.args(args)
            .current_dir(cwd)
            .env_remove("CLAUDECODE")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Agent(format!("failed to spawn agent: {e}")))?;

        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Agent("no stdin".into()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Agent("no stdout".into()))?;

        Ok(Self {
            child,
            writer: Some(BufWriter::new(stdin)),
            reader: BufReader::new(stdout),
        })
    }

    /// Write raw text to stdin and close it (drop the writer to close the pipe).
    pub async fn write_and_close_stdin(&mut self, text: &str) -> Result<()> {
        let writer = self
            .writer
            .as_mut()
            .ok_or_else(|| Error::Agent("stdin already closed".into()))?;
        writer
            .write_all(text.as_bytes())
            .await
            .map_err(|e| Error::Agent(format!("write error: {e}")))?;
        writer
            .flush()
            .await
            .map_err(|e| Error::Agent(format!("flush error: {e}")))?;
        // Drop the writer to close the pipe — this signals EOF to the child
        self.writer = None;
        Ok(())
    }

    /// Read the next JSON message from stdout. Returns None if the process exited.
    pub async fn recv(&mut self) -> Result<Option<Value>> {
        loop {
            let mut line = String::new();
            let n = self
                .reader
                .read_line(&mut line)
                .await
                .map_err(|e| Error::Agent(format!("read error: {e}")))?;
            if n == 0 {
                // Process closed stdout — try to capture stderr for diagnostics
                if let Some(stderr) = self.child.stderr.as_mut() {
                    let mut buf = String::new();
                    let mut stderr_reader = tokio::io::BufReader::new(stderr);
                    while stderr_reader.read_line(&mut buf).await.unwrap_or(0) > 0 {}
                    if !buf.is_empty() {
                        tracing::error!(stderr = %buf.trim(), "agent process stderr");
                    }
                }
                return Ok(None);
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            tracing::trace!(line = %trimmed, "agent stdout");
            let value = serde_json::from_str(trimmed)
                .map_err(|e| Error::AgentProtocol(format!("parse error on `{}`: {e}", &trimmed[..trimmed.len().min(100)])))?;
            return Ok(Some(value));
        }
    }

    /// Kill the process.
    pub async fn kill(&mut self) -> Result<()> {
        self.child
            .kill()
            .await
            .map_err(|e| Error::Agent(format!("kill error: {e}")))?;
        Ok(())
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}
