use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

pub mod cargo;
pub mod pytest;
pub mod ruff;
pub mod ruff_format;
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

/// RAII guard that unregisters a PID from `ACTIVE_HOOK_PIDS` on drop.
///
/// Without this, an error path in `run_with_timeout` (e.g. `child.try_wait()`
/// returning `Err`) would propagate via `?` and leave the PID registered
/// forever. A later `kill_all_active_hooks()` could then target a recycled
/// PID belonging to an unrelated process — see audit P0-2.
struct HookPidGuard(u32);

impl Drop for HookPidGuard {
    fn drop(&mut self) {
        unregister_hook_pid(self.0);
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
    // Drop guard guarantees unregistration on every return path including
    // ?-propagated errors. Created AFTER successful register so a future
    // panic between register and guard creation cannot leave a stranded PID.
    let _pid_guard = HookPidGuard(pid);

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

/// The two parallel vectors `build_hook_list` produces: the live adapters
/// that the orchestrator will run, and the `ToolInfo` records that get
/// written into `state.json` so consumers know which binaries pulci resolved.
pub type BuiltHooks = (
    Vec<std::sync::Arc<dyn Hook>>,
    Vec<crate::state::ToolInfo>,
);

/// Build the active hook adapter list and the matching `ToolInfo` vector
/// from a parsed `pulci.toml` configuration.
///
/// The function only mounts adapters whose flag is `true` in `hooks_cfg`,
/// so disabled tools never pay the cold-start cost of a `--version` probe.
/// `ruff` and `ruff_format` share the same binary — when at least one of
/// the two flags is true, the ruff tool is resolved exactly once and the
/// resulting `ToolInfo` appears exactly once in the output.
///
/// `resolve` is an injected closure with signature
/// `FnMut(&'static str, Option<&str>) -> anyhow::Result<ResolvedTool>`.
/// In production the daemon passes a closure that calls
/// [`crate::resolver::resolve_tool`] against the project root. Tests can
/// substitute a stub that returns synthetic `ResolvedTool` values without
/// spawning subprocesses.
pub fn build_hook_list(
    hooks_cfg: &crate::config::HooksConfig,
    tools_cfg: &crate::config::ToolsConfig,
    project_root: &std::path::Path,
    hook_timeout: Duration,
    mut resolve: impl FnMut(&'static str, Option<&str>) -> anyhow::Result<crate::resolver::ResolvedTool>,
) -> anyhow::Result<BuiltHooks> {
    let mut hooks: Vec<std::sync::Arc<dyn Hook>> = Vec::new();
    let mut tool_infos: Vec<crate::state::ToolInfo> = Vec::new();

    // ruff and ruff_format share the same binary — resolve once, mount both.
    if hooks_cfg.ruff || hooks_cfg.ruff_format {
        let r = resolve("ruff", tools_cfg.ruff.as_deref())?;
        tool_infos.push(r.to_tool_info());
        if hooks_cfg.ruff {
            hooks.push(std::sync::Arc::new(ruff::RuffAdapter::new(&r, hook_timeout)));
        }
        if hooks_cfg.ruff_format {
            hooks.push(std::sync::Arc::new(ruff_format::RuffFormatAdapter::new(
                &r,
                hook_timeout,
            )));
        }
    }
    if hooks_cfg.ty {
        let r = resolve("ty", tools_cfg.ty.as_deref())?;
        tool_infos.push(r.to_tool_info());
        hooks.push(std::sync::Arc::new(ty::TyAdapter::new(&r, hook_timeout)));
    }
    if hooks_cfg.pytest {
        let r = resolve("pytest", tools_cfg.pytest.as_deref())?;
        tool_infos.push(r.to_tool_info());
        hooks.push(std::sync::Arc::new(pytest::PytestAdapter::new(
            &r,
            project_root.to_path_buf(),
            hook_timeout,
            hooks_cfg.pytest_test_patterns.clone(),
        )));
    }
    if hooks_cfg.clippy {
        let r = resolve("cargo", None)?;
        tool_infos.push(r.to_tool_info());
        hooks.push(std::sync::Arc::new(cargo::CargoAdapter::new(&r, hook_timeout)));
    }

    Ok((hooks, tool_infos))
}

#[cfg(test)]
mod timeout_tests {
    use super::*;

    /// Serializes tests that interact with `ACTIVE_HOOK_PIDS` or
    /// `kill_all_active_hooks()`. Cargo runs `#[test]`s in parallel by default,
    /// and tests that register PIDs would otherwise race with the
    /// `kill_all_*` tests — the kill iterator would send SIGTERM to a child
    /// owned by another test, masking timeout-induced failures as
    /// signal-induced exits. Acquire this lock at the top of any test that
    /// touches the registry.
    #[cfg(unix)]
    static HOOK_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[cfg(unix)]
    fn hook_test_guard() -> std::sync::MutexGuard<'static, ()> {
        HOOK_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_hung_process() {
        let _guard = hook_test_guard();
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
        let _guard = hook_test_guard();
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
        let _guard = hook_test_guard();
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
        let _guard = hook_test_guard();
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
        let _guard = hook_test_guard();
        // No panic, no error. Safe to call without any registered hooks.
        kill_all_active_hooks();
    }
}

#[cfg(test)]
mod build_hook_list_tests {
    use super::*;
    use crate::config::{HooksConfig, ToolsConfig};
    use crate::resolver::{ResolvedTool, ToolSource};
    use std::cell::RefCell;
    use std::path::Path;
    use std::time::Duration;

    const TIMEOUT: Duration = Duration::from_secs(60);

    fn fake_resolve(
        name: &'static str,
        _pinned: Option<&str>,
    ) -> anyhow::Result<ResolvedTool> {
        Ok(ResolvedTool {
            name,
            version: "0.0.0-test".to_string(),
            source: ToolSource::UvxLatest,
            invocation: vec!["uvx".into(), name.into()],
        })
    }

    fn names_of(tool_infos: &[crate::state::ToolInfo]) -> Vec<&str> {
        tool_infos.iter().map(|t| t.name.as_str()).collect()
    }

    fn hook_names(hooks: &[std::sync::Arc<dyn Hook>]) -> Vec<&'static str> {
        hooks.iter().map(|h| h.name()).collect()
    }

    #[test]
    fn empty_config_yields_no_hooks() {
        // All flags off — daemon would run with no adapters mounted.
        let cfg = HooksConfig {
            ruff: false,
            ruff_format: false,
            ty: false,
            pytest: false,
            clippy: false,
            timeout_secs: None,
            pytest_test_patterns: Vec::new(),
        };
        let tools = ToolsConfig::default();
        let (hooks, tool_infos) =
            build_hook_list(&cfg, &tools, Path::new("/tmp"), TIMEOUT, fake_resolve).unwrap();
        assert!(hooks.is_empty());
        assert!(tool_infos.is_empty());
    }

    #[test]
    fn default_config_mounts_ruff_and_ty() {
        // HooksConfig::default() = ruff=true, ty=true, everything else off.
        let cfg = HooksConfig::default();
        let tools = ToolsConfig::default();
        let (hooks, tool_infos) =
            build_hook_list(&cfg, &tools, Path::new("/tmp"), TIMEOUT, fake_resolve).unwrap();
        assert_eq!(hook_names(&hooks), vec!["ruff", "ty"]);
        assert_eq!(names_of(&tool_infos), vec!["ruff", "ty"]);
    }

    #[test]
    fn ruff_and_ruff_format_share_a_single_resolution() {
        // Both flags on → ruff binary is resolved once; tool_infos has one
        // ruff entry but two adapters appear in the hook list.
        let mut cfg = HooksConfig::default();
        cfg.ruff = true;
        cfg.ruff_format = true;
        cfg.ty = false;
        let tools = ToolsConfig::default();

        let calls: RefCell<Vec<&'static str>> = RefCell::new(Vec::new());
        let resolver = |name: &'static str, pinned: Option<&str>| {
            calls.borrow_mut().push(name);
            fake_resolve(name, pinned)
        };
        let (hooks, tool_infos) =
            build_hook_list(&cfg, &tools, Path::new("/tmp"), TIMEOUT, resolver).unwrap();

        assert_eq!(hook_names(&hooks), vec!["ruff", "ruff_format"]);
        assert_eq!(names_of(&tool_infos), vec!["ruff"]);
        assert_eq!(calls.into_inner(), vec!["ruff"], "ruff resolved once for both adapters");
    }

    #[test]
    fn ruff_format_alone_does_not_mount_ruff_check() {
        // Format-only configuration: ruff still resolved (shared binary)
        // but only the format adapter is mounted.
        let mut cfg = HooksConfig::default();
        cfg.ruff = false;
        cfg.ruff_format = true;
        cfg.ty = false;
        let tools = ToolsConfig::default();
        let (hooks, tool_infos) =
            build_hook_list(&cfg, &tools, Path::new("/tmp"), TIMEOUT, fake_resolve).unwrap();
        assert_eq!(hook_names(&hooks), vec!["ruff_format"]);
        assert_eq!(names_of(&tool_infos), vec!["ruff"]);
    }

    #[test]
    fn all_hooks_enabled_mounts_five_adapters_and_four_tool_infos() {
        let cfg = HooksConfig {
            ruff: true,
            ruff_format: true,
            ty: true,
            pytest: true,
            clippy: true,
            timeout_secs: None,
            pytest_test_patterns: Vec::new(),
        };
        let tools = ToolsConfig::default();
        let (hooks, tool_infos) =
            build_hook_list(&cfg, &tools, Path::new("/tmp"), TIMEOUT, fake_resolve).unwrap();
        assert_eq!(
            hook_names(&hooks),
            vec!["ruff", "ruff_format", "ty", "pytest", "clippy"]
        );
        // ruff/ruff_format share one entry; ty, pytest, clippy each their own.
        assert_eq!(names_of(&tool_infos), vec!["ruff", "ty", "pytest", "cargo"]);
    }

    #[test]
    fn pinned_versions_in_config_reach_resolver() {
        // Each pinned version in ToolsConfig must be passed to the resolver
        // closure as the `pinned` argument for the matching tool.
        let mut cfg = HooksConfig::default();
        cfg.pytest = true;
        cfg.clippy = false;
        let tools = ToolsConfig {
            ruff: Some("0.7.4".into()),
            ty: Some("0.0.3".into()),
            pytest: Some("8.3.0".into()),
        };

        let observed: RefCell<Vec<(&'static str, Option<String>)>> = RefCell::new(Vec::new());
        let resolver = |name: &'static str, pinned: Option<&str>| {
            observed
                .borrow_mut()
                .push((name, pinned.map(String::from)));
            fake_resolve(name, pinned)
        };
        let _ = build_hook_list(&cfg, &tools, Path::new("/tmp"), TIMEOUT, resolver).unwrap();

        assert_eq!(
            observed.into_inner(),
            vec![
                ("ruff", Some("0.7.4".into())),
                ("ty", Some("0.0.3".into())),
                ("pytest", Some("8.3.0".into())),
            ]
        );
    }

    #[test]
    fn resolver_error_propagates() {
        // A failure in the resolver (e.g. uvx unavailable, pinned version
        // not found) must abort hook construction with the same error —
        // never silently produce a partial hook list.
        let cfg = HooksConfig::default();
        let tools = ToolsConfig::default();
        let resolver = |_name: &'static str, _pinned: Option<&str>| {
            anyhow::bail!("synthetic resolver failure")
        };
        // Arc<dyn Hook> isn't Debug, so unwrap/expect_err on the result type
        // won't compile. Match manually.
        let err = match build_hook_list(&cfg, &tools, Path::new("/tmp"), TIMEOUT, resolver) {
            Ok(_) => panic!("resolver error should propagate"),
            Err(e) => e,
        };
        assert!(
            err.to_string().contains("synthetic resolver failure"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn cargo_hook_resolves_with_no_pin_even_if_tools_has_no_cargo_field() {
        // clippy is invoked by resolving "cargo" with no pinned version
        // available in ToolsConfig (which doesn't expose a cargo pin slot).
        // The resolver must be called with `None` regardless of other pins.
        let mut cfg = HooksConfig::default();
        cfg.ruff = false;
        cfg.ty = false;
        cfg.clippy = true;
        let tools = ToolsConfig {
            ruff: Some("0.7.4".into()),
            ty: None,
            pytest: None,
        };

        let observed: RefCell<Vec<(&'static str, Option<String>)>> = RefCell::new(Vec::new());
        let resolver = |name: &'static str, pinned: Option<&str>| {
            observed
                .borrow_mut()
                .push((name, pinned.map(String::from)));
            fake_resolve(name, pinned)
        };
        let _ = build_hook_list(&cfg, &tools, Path::new("/tmp"), TIMEOUT, resolver).unwrap();

        assert_eq!(observed.into_inner(), vec![("cargo", None)]);
    }
}
