use crate::error::{Error, Result};
use serde_json::Value;
use std::path::Path;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, BufWriter};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

pub struct AgentProcess {
    child: Child,
    writer: BufWriter<ChildStdin>,
    reader: BufReader<ChildStdout>,
}

impl AgentProcess {
    /// Spawn the agent subprocess.
    pub async fn spawn(command: &str, cwd: &Path) -> Result<Self> {
        let parts: Vec<&str> = command.split_whitespace().collect();
        let (program, args) = parts
            .split_first()
            .ok_or_else(|| Error::Agent("empty agent command".into()))?;

        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
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
            writer: BufWriter::new(stdin),
            reader: BufReader::new(stdout),
        })
    }

    /// Send a JSON message (line-delimited).
    pub async fn send(&mut self, msg: &Value) -> Result<()> {
        let mut line = serde_json::to_string(msg)
            .map_err(|e| Error::AgentProtocol(format!("serialize error: {e}")))?;
        line.push('\n');
        self.writer
            .write_all(line.as_bytes())
            .await
            .map_err(|e| Error::Agent(format!("write error: {e}")))?;
        self.writer
            .flush()
            .await
            .map_err(|e| Error::Agent(format!("flush error: {e}")))?;
        Ok(())
    }

    /// Read the next JSON message from stdout. Returns None if the process exited.
    pub async fn recv(&mut self) -> Result<Option<Value>> {
        let mut line = String::new();
        let n = self
            .reader
            .read_line(&mut line)
            .await
            .map_err(|e| Error::Agent(format!("read error: {e}")))?;
        if n == 0 {
            return Ok(None);
        }
        let value = serde_json::from_str(line.trim())
            .map_err(|e| Error::AgentProtocol(format!("parse error on line: {e}")))?;
        Ok(Some(value))
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
