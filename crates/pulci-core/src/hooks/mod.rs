use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub mod cargo;
pub mod pytest;
pub mod ruff;
pub mod ty;

/// Wall-clock budget for a single hook subprocess.
///
/// At 120s the daemon stays unblocked under cold pytest / first-run clippy
/// (both of which can legitimately take ~30-60s) but still surfaces a clear
/// failure when a tool hangs indefinitely (uvx network stall, deadlocked
/// child, infinite-loop user code under pytest).
pub const DEFAULT_HOOK_TIMEOUT: Duration = Duration::from_secs(120);

const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Spawn `cmd`, drain its stdout/stderr in background threads, and wait up to
/// `timeout`. If the child has not exited by then it is killed and an error
/// is returned.
///
/// Returns an `Output` equivalent to `Command::output()` on success.
pub fn run_with_timeout(
    mut cmd: Command,
    timeout: Duration,
    name: &str,
) -> anyhow::Result<Output> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    let stdout_reader = child.stdout.take().map(spawn_drainer);
    let stderr_reader = child.stderr.take().map(spawn_drainer);

    let start = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if start.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            anyhow::bail!(
                "{name} timed out after {}s and was killed by pulci",
                timeout.as_secs()
            );
        }
        std::thread::sleep(POLL_INTERVAL);
    };

    let stdout = stdout_reader
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_reader
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

fn spawn_drainer<R: Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = reader.read_to_end(&mut buf);
        buf
    })
}

/// Normalized severity level shared across all hook adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Info => write!(f, "info"),
        }
    }
}

/// A single diagnostic emitted by a quality-gate tool.
///
/// This is the unified type that every hook adapter must produce.
/// Day 4 will aggregate these into `.pulci/state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub tool: String,
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
    pub severity: Severity,
    pub code: Option<String>,
    pub message: String,
}

/// A quality-gate tool adapter.
///
/// Each adapter shells out to its canonical tool and normalises the output
/// into `Vec<Diagnostic>`. Adding a new tool means implementing this trait
/// in a new file — nothing else changes.
pub trait Hook: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>>;
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_hung_process() {
        let mut cmd = Command::new("sleep");
        cmd.arg("5");
        let result = run_with_timeout(cmd, Duration::from_millis(200), "sleep-test");
        let err = result.expect_err("expected timeout error");
        let msg = err.to_string();
        assert!(msg.contains("timed out"), "unexpected error: {msg}");
        assert!(msg.contains("sleep-test"), "expected tool name in error: {msg}");
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_returns_output_when_fast() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf hello && printf world >&2"]);
        let output = run_with_timeout(cmd, Duration::from_secs(5), "sh-test").unwrap();
        assert!(output.status.success());
        assert_eq!(output.stdout, b"hello");
        assert_eq!(output.stderr, b"world");
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_propagates_spawn_failure() {
        let cmd = Command::new("__pulci_nonexistent_binary_xyz__");
        let result = run_with_timeout(cmd, Duration::from_secs(1), "missing");
        assert!(result.is_err(), "expected spawn failure to propagate");
    }
}
