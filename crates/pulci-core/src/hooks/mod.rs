use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub mod cargo;
pub mod pytest;
pub mod ruff;
pub mod ty;

/// Registry of hook subprocess PIDs currently being awaited by `run_with_timeout`.
///
/// On Unix, the daemon installs a SIGTERM handler that drains this list and
/// signals each child so they don't outlive the daemon as orphans. The default
/// foreground-process-group propagation already covers terminal Ctrl-C (SIGINT
/// reaches the whole pgid); this registry is specifically for external SIGTERM
/// from `kill`, systemd, or supervising scripts where only the daemon receives
/// the signal.
static ACTIVE_HOOK_PIDS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

fn register_hook_pid(pid: u32) {
    if let Ok(mut v) = ACTIVE_HOOK_PIDS.lock() {
        v.push(pid);
    }
}

fn unregister_hook_pid(pid: u32) {
    if let Ok(mut v) = ACTIVE_HOOK_PIDS.lock() {
        v.retain(|p| *p != pid);
    }
}

/// Send SIGTERM to every hook subprocess currently registered as active.
///
/// Called from the daemon's SIGTERM handler to prevent orphan ruff/ty/pytest/
/// clippy processes outliving their parent. Sending to a no-longer-existing
/// PID is harmless (returns ESRCH).
#[cfg(unix)]
pub fn kill_all_active_hooks() {
    let Ok(pids) = ACTIVE_HOOK_PIDS.lock() else {
        return;
    };
    for &pid in pids.iter() {
        // SAFETY: `libc::kill` is async-signal-safe. The caller is the main
        // thread (signal-hook dispatches signals out-of-handler), so locking
        // the mutex above is also safe.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}

#[cfg(not(unix))]
pub fn kill_all_active_hooks() {
    // No-op on non-Unix; Windows has different process-lifecycle semantics
    // and is not handled in this pass.
}

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

    let pid = child.id();
    register_hook_pid(pid);

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
            unregister_hook_pid(pid);
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

    unregister_hook_pid(pid);

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

    #[cfg(unix)]
    #[test]
    fn register_unregister_pid_round_trip() {
        // Use a synthetic PID that is extremely unlikely to collide with a real
        // process; the registry doesn't validate, so this exercises the data
        // structure mechanics without touching the OS.
        let synthetic = 0xDEAD_BEEF_u32;
        register_hook_pid(synthetic);
        assert!(ACTIVE_HOOK_PIDS.lock().unwrap().contains(&synthetic));
        unregister_hook_pid(synthetic);
        assert!(!ACTIVE_HOOK_PIDS.lock().unwrap().contains(&synthetic));
    }

    #[cfg(unix)]
    #[test]
    fn kill_all_active_hooks_terminates_a_real_child() {
        // The acid test: spawn a long-running subprocess, register it, fire
        // kill_all, expect the OS to mark it as killed-by-signal.
        let mut child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        let pid = child.id();
        register_hook_pid(pid);

        kill_all_active_hooks();

        // SIGTERM delivery is async; poll for up to 2s for the child to exit.
        let start = Instant::now();
        let status = loop {
            if let Ok(Some(status)) = child.try_wait() {
                break status;
            }
            if start.elapsed() > Duration::from_secs(2) {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child did not exit after kill_all_active_hooks()");
            }
            std::thread::sleep(Duration::from_millis(50));
        };

        unregister_hook_pid(pid);
        assert!(
            !status.success(),
            "child should be killed by signal, got success"
        );
    }

    #[cfg(unix)]
    #[test]
    fn kill_all_active_hooks_empty_registry_is_noop() {
        // No panic, no error. Safe to call without any registered hooks.
        kill_all_active_hooks();
    }
}
