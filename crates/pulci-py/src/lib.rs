//! pulci-py
//!
//! PyO3 bindings. This crate is the bridge between `pulci-core` (pure Rust)
//! and the Python CLI package. It compiles to a native module imported as
//! `pulci._native` from Python.

// PyO3's `?` on PyResult<()> → PyResult<()> triggers From<PyErr> for PyErr,
// which is a no-op identity conversion. Suppress it crate-wide.
#![allow(clippy::useless_conversion)]

use std::collections::HashSet;
use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::PathBuf;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use fs2::FileExt;
use pyo3::prelude::*;

fn write_heartbeat(path: &Path) {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let now = pulci_core::state::now_iso8601();
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, now).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}
use pulci_core::cache::FileCache;
use pulci_core::config::load_config;
use pulci_core::hooks::cargo::CargoAdapter;
use pulci_core::hooks::pytest::PytestAdapter;
use pulci_core::hooks::ruff::RuffAdapter;
use pulci_core::hooks::ty::TyAdapter;
use pulci_core::hooks::Hook;
use pulci_core::orchestrator::Orchestrator;
use pulci_core::state::{build_state, read_state, write_state};
use pulci_core::watcher::{watch, FileEvent, WatcherConfig};

fn resolved_to_info(rt: &pulci_core::resolver::ResolvedTool) -> pulci_core::state::ToolInfo {
    use pulci_core::resolver::ToolSource;
    let (source, path) = match &rt.source {
        ToolSource::Pinned { .. } => ("pinned".into(), None),
        ToolSource::LocalVenv { path } => ("local-venv".into(), Some(path.display().to_string())),
        ToolSource::SystemPath { path } => ("system-path".into(), Some(path.display().to_string())),
        ToolSource::UvxLatest => ("uvx-latest".into(), None),
    };
    pulci_core::state::ToolInfo {
        name: rt.name.to_owned(),
        version: rt.version.clone(),
        source,
        path,
    }
}

/// Returns the pulci-core version string.
#[pyfunction]
fn version() -> &'static str {
    pulci_core::version()
}

fn is_excluded(path: &Path, project_root: &Path, excludes: &[String]) -> bool {
    excludes.iter().any(|excl| path.starts_with(project_root.join(excl)))
}

/// Collect all `.py` files under `root`, skipping ignored and excluded directories.
fn collect_py_files(root: &Path, project_root: &Path, excludes: &[String]) -> Vec<PathBuf> {
    let mut result = Vec::new();
    collect_py_files_inner(root, project_root, excludes, &mut result);
    result
}

fn collect_py_files_inner(dir: &Path, project_root: &Path, excludes: &[String], result: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if pulci_core::watcher::is_ignored(&path) {
            continue;
        }
        if is_excluded(&path, project_root, excludes) {
            continue;
        }
        if path.is_dir() {
            collect_py_files_inner(&path, project_root, excludes, result);
        } else if path.extension().map(|e| e == "py").unwrap_or(false) {
            result.push(path);
        }
    }
}

/// Watch `path` for changes, run quality hooks, and write `.pulci/state.json`.
///
/// When `agent` is true, each check event is printed as compiler-style diagnostics
/// instead of human-readable text — suitable for machine consumption.
///
/// Blocks until Ctrl-C (raises KeyboardInterrupt via `py.check_signals()`).
#[pyfunction]
fn start(py: Python<'_>, path: String, agent: bool) -> PyResult<()> {
    let project_root = PathBuf::from(&path);
    let pulci_dir = project_root.join(".pulci");
    let state_file = pulci_dir.join("state.json");

    // Acquire an advisory exclusive lock on .pulci/daemon.lock so a second
    // `pulci start` against the same project fails fast instead of racing on
    // state.json / heartbeat. The kernel releases the lock when this process
    // exits (clean or via signal); the `_lock_file` binding keeps the FD alive
    // for the lifetime of start().
    std::fs::create_dir_all(&pulci_dir)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(
            format!("failed to create .pulci directory: {e}")
        ))?;
    let lock_path = pulci_dir.join("daemon.lock");
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(
            format!("failed to open lock file {}: {e}", lock_path.display())
        ))?;
    if let Err(e) = lock_file.try_lock_exclusive() {
        return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
            "another pulci daemon is already running for this project (lock: {}): {e}",
            lock_path.display()
        )));
    }
    let _lock_file = lock_file;

    let config = load_config(&project_root).unwrap_or_else(|e| {
        eprintln!("pulci: warning: failed to load pulci.toml ({e}), using defaults");
        Default::default()
    });

    // Resolve only the tools that are enabled — disabled tools are skipped
    // entirely so their cold-start cost (uvx download, --version probe) is
    // never paid.
    let mut hook_list: Vec<Arc<dyn Hook>> = Vec::new();
    let mut tool_infos: Vec<pulci_core::state::ToolInfo> = Vec::new();

    if config.hooks.ruff {
        let r = pulci_core::resolver::resolve_tool("ruff", &project_root, config.tools.ruff.as_deref())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        tool_infos.push(resolved_to_info(&r));
        hook_list.push(Arc::new(RuffAdapter::new(&r)));
    }
    if config.hooks.ty {
        let r = pulci_core::resolver::resolve_tool("ty", &project_root, config.tools.ty.as_deref())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        tool_infos.push(resolved_to_info(&r));
        hook_list.push(Arc::new(TyAdapter::new(&r)));
    }
    if config.hooks.pytest {
        let r = pulci_core::resolver::resolve_tool("pytest", &project_root, config.tools.pytest.as_deref())
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        tool_infos.push(resolved_to_info(&r));
        hook_list.push(Arc::new(PytestAdapter::new(&r, project_root.clone())));
    }
    if config.hooks.clippy {
        let r = pulci_core::resolver::resolve_tool("cargo", &project_root, None)
            .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;
        tool_infos.push(resolved_to_info(&r));
        hook_list.push(Arc::new(CargoAdapter::new(&r)));
    }

    // Determine stale: tools changed since last daemon run?
    let mut stale = read_state(&state_file)
        .map_err(|e| eprintln!("pulci: warning: could not read prior state for stale check: {e}"))
        .ok()
        .is_some_and(|prev| pulci_core::state::tools_changed(&prev.tools, &tool_infos));

    let heartbeat_path = project_root.join(".pulci").join("heartbeat");
    let project_root_for_scan = project_root.clone();
    let config_watcher = WatcherConfig { path: project_root };
    let (tx, rx) = mpsc::channel();
    let (watcher_err_tx, watcher_err_rx) = mpsc::channel::<String>();
    let (ready_tx, ready_rx) = mpsc::channel::<()>();

    std::thread::spawn(move || {
        if let Err(e) = watch(config_watcher, tx, ready_tx) {
            let _ = watcher_err_tx.send(e.to_string());
        }
    });

    // Wait until inotify watches are registered before printing "Watching".
    // This ensures piped readers (tests, CI) get the message only after
    // the watcher is genuinely ready to receive events.
    let _ = ready_rx.recv_timeout(Duration::from_secs(5));

    if !agent {
        if !tool_infos.is_empty() {
            let summary = tool_infos
                .iter()
                .map(|t| format!("{}={} ({})", t.name, t.version, t.source))
                .collect::<Vec<_>>()
                .join(", ");
            println!("resolved: {summary}");
        }
        println!("Watching {path} — press Ctrl-C to stop.");
        let _ = std::io::stdout().flush();
    }

    // Heartbeat thread: writes .pulci/heartbeat every 10s while the daemon is alive.
    // pulci status derives daemon_status from the age of this file — no PID files,
    // no race conditions. If the daemon dies for any reason, heartbeats stop and
    // status correctly reports "dead" after 120s.
    let heartbeat_stop = Arc::new(AtomicBool::new(false));
    let heartbeat_stop_clone = Arc::clone(&heartbeat_stop);
    std::thread::spawn(move || {
        write_heartbeat(&heartbeat_path);
        loop {
            std::thread::sleep(Duration::from_secs(10));
            if heartbeat_stop_clone.load(Ordering::Relaxed) {
                break;
            }
            write_heartbeat(&heartbeat_path);
        }
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    let orchestrator = Orchestrator::new(hook_list);
    let mut cache = FileCache::new();

    // Initial full-project scan so state.json exists before the first file event.
    {
        let all_py = collect_py_files(&project_root_for_scan, &project_root_for_scan, &config.watch.exclude);
        let changed = cache.filter_changed(&all_py);
        if !changed.is_empty() {
            let t0 = Instant::now();
            let (results, state) = py.allow_threads(|| {
                let r = rt.block_on(orchestrator.run(&changed));
                let s = build_state(&r, tool_infos.clone(), stale);
                (r, s)
            });
            let _ = results;
            stale = false;
            for d in &state.diagnostics {
                let code_part = d.code.as_deref()
                    .map(|c| format!("[{}/{}]", d.tool, c))
                    .unwrap_or_else(|| format!("[{}]", d.tool));
                println!(
                    "{}:{}:{}: {} {} {}",
                    d.file.display(), d.line, d.col, d.severity, code_part, d.message
                );
            }
            let elapsed = t0.elapsed().as_secs_f64();
            println!(
                "{} errors, {} warnings ({} files checked, {:.1}s)",
                state.summary.errors,
                state.summary.warnings,
                changed.len(),
                elapsed,
            );
            if let Err(e) = write_state(&state_file, &state) {
                eprintln!("pulci: failed to write initial state: {e}");
            }
        }
    }

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(first) => {
                let mut paths: HashSet<PathBuf> = HashSet::new();
                let mut needs_rescan = false;
                match first {
                    FileEvent::Rescan => { needs_rescan = true; }
                    FileEvent::Changed { path, .. } => { paths.insert(path); }
                }
                let deadline = Instant::now() + Duration::from_millis(50);
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(FileEvent::Rescan) => { needs_rescan = true; }
                        Ok(FileEvent::Changed { path, .. }) => { paths.insert(path); }
                        Err(_) => break,
                    }
                }

                let files: Vec<PathBuf> = if needs_rescan {
                    // Watcher reported lost events — re-collect every source file under
                    // the project root and let the cache filter the actually-changed ones.
                    // Identical surface to the startup scan; orchestrator handles the rest.
                    collect_py_files(
                        &project_root_for_scan,
                        &project_root_for_scan,
                        &config.watch.exclude,
                    )
                } else {
                    paths
                        .into_iter()
                        .filter(|p| !is_excluded(p, &project_root_for_scan, &config.watch.exclude))
                        .filter(|p| {
                            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
                            ext == "py" || (config.hooks.clippy && ext == "rs")
                        })
                        .collect()
                };
                let changed = cache.filter_changed(&files);
                if changed.is_empty() {
                    continue;
                }

                let t0 = Instant::now();
                let (results, state) = py.allow_threads(|| {
                    let r = rt.block_on(orchestrator.run(&changed));
                    let s = build_state(&r, tool_infos.clone(), stale);
                    (r, s)
                });
                let _ = results;
                stale = false; // only first run after tool change is stale
                py.check_signals()?;

                // Compiler-style output per FORMATS.md grammar:
                // <file>:<line>:<col>: <severity>[<scope>/<code>] <message>
                for d in &state.diagnostics {
                    let code_part = d.code.as_deref()
                        .map(|c| format!("[{}/{}]", d.tool, c))
                        .unwrap_or_else(|| format!("[{}]", d.tool));
                    println!(
                        "{}:{}:{}: {} {} {}",
                        d.file.display(), d.line, d.col, d.severity, code_part, d.message
                    );
                }
                let elapsed = t0.elapsed().as_secs_f64();
                println!(
                    "{} errors, {} warnings ({} files checked, {:.1}s)",
                    state.summary.errors,
                    state.summary.warnings,
                    changed.len(),
                    elapsed,
                );

                if let Err(e) = write_state(&state_file, &state) {
                    eprintln!("pulci: failed to write state: {e}");
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                py.check_signals()?;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if let Ok(err) = watcher_err_rx.try_recv() {
                    return Err(pyo3::exceptions::PyRuntimeError::new_err(
                        format!("pulci watcher failed: {err}"),
                    ));
                }
                break;
            }
        }
    }

    heartbeat_stop.store(true, Ordering::Relaxed);
    Ok(())
}

/// The native module mounted as `pulci._native` in Python.
///
/// The function name MUST match `[lib] name` in Cargo.toml — both are
/// `_native`. PyO3 generates `PyInit__native` from this function,
/// which is what Python looks up when importing `pulci._native`.
#[pymodule]
fn _native(_py: Python<'_>, m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(version, m)?)?;
    m.add_function(wrap_pyfunction!(start, m)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_tmp_lock() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("pulci_lock_test_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("daemon.lock")
    }

    #[test]
    fn second_try_lock_on_same_path_fails() {
        let lock_path = unique_tmp_lock();

        let first = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        first.try_lock_exclusive().unwrap();

        let second = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        let result = second.try_lock_exclusive();
        assert!(
            result.is_err(),
            "expected second try_lock_exclusive to fail while first lock is held"
        );

        // Drop the first lock; a fresh acquisition should now succeed.
        drop(first);
        let third = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .unwrap();
        third.try_lock_exclusive().unwrap();

        std::fs::remove_dir_all(lock_path.parent().unwrap()).ok();
    }
}
