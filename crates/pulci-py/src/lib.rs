//! pulci-py
//!
//! PyO3 bindings. This crate is the bridge between `pulci-core` (pure Rust)
//! and the Python CLI package. It compiles to a native module imported as
//! `pulci._native` from Python.

// PyO3's `?` on PyResult<()> → PyResult<()> triggers From<PyErr> for PyErr,
// which is a no-op identity conversion. Suppress it crate-wide.
#![allow(clippy::useless_conversion)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use pyo3::prelude::*;
use pulci_core::cache::FileCache;
use pulci_core::config::load_config;
use pulci_core::hooks::pytest::PytestAdapter;
use pulci_core::hooks::ruff::RuffAdapter;
use pulci_core::hooks::ty::TyAdapter;
use pulci_core::hooks::Hook;
use pulci_core::orchestrator::Orchestrator;
use pulci_core::state::{build_state, write_state};
use pulci_core::watcher::{watch, WatcherConfig};

#[derive(serde::Serialize)]
struct CheckEvent {
    event: &'static str,
    files: usize,
    errors: u32,
    warnings: u32,
    checks_run: u32,
    stale: bool,
}

/// Returns the pulci-core version string.
#[pyfunction]
fn version() -> &'static str {
    pulci_core::version()
}

/// Watch `path` for changes, run quality hooks, and write `.pulci/state.json`.
///
/// When `agent` is true, each check event is printed as a single JSON line
/// instead of human-readable text — suitable for machine consumption.
///
/// Blocks until Ctrl-C (raises KeyboardInterrupt via `py.check_signals()`).
#[pyfunction]
fn start(py: Python<'_>, path: String, agent: bool) -> PyResult<()> {
    let project_root = PathBuf::from(&path);
    let state_file = project_root.join(".pulci").join("state.json");

    let config = load_config(&project_root).unwrap_or_else(|e| {
        eprintln!("pulci: warning: failed to load pulci.toml ({e}), using defaults");
        Default::default()
    });

    let mut hook_list: Vec<Arc<dyn Hook>> = Vec::new();
    if config.hooks.ruff {
        hook_list.push(Arc::new(RuffAdapter));
    }
    if config.hooks.ty {
        hook_list.push(Arc::new(TyAdapter));
    }
    if config.hooks.pytest {
        hook_list.push(Arc::new(PytestAdapter));
    }

    let config_watcher = WatcherConfig {
        path: project_root,
    };
    let (tx, rx) = mpsc::channel();

    let (watcher_err_tx, watcher_err_rx) = mpsc::channel::<String>();
    std::thread::spawn(move || {
        if let Err(e) = watch(config_watcher, tx) {
            let _ = watcher_err_tx.send(e.to_string());
        }
    });

    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .map_err(|e| pyo3::exceptions::PyRuntimeError::new_err(e.to_string()))?;

    let orchestrator = Orchestrator::new(hook_list);
    let mut cache = FileCache::new();

    loop {
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(first) => {
                // Debounce: collect all events arriving within 50 ms.
                let mut paths: HashSet<PathBuf> = HashSet::new();
                paths.insert(first.path);
                let deadline = Instant::now() + Duration::from_millis(50);
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(e) => {
                            paths.insert(e.path);
                        }
                        Err(_) => break,
                    }
                }

                let files: Vec<PathBuf> = paths.into_iter().collect();
                let changed = cache.filter_changed(&files);
                if changed.is_empty() {
                    continue;
                }

                let (_results, state) = py.allow_threads(|| {
                    let r = rt.block_on(orchestrator.run(&changed));
                    let s = build_state(&r);
                    (r, s)
                });
                py.check_signals()?;

                if agent {
                    let ev = CheckEvent {
                        event: "check",
                        files: changed.len(),
                        errors: state.summary.errors,
                        warnings: state.summary.warnings,
                        checks_run: state.summary.checks_run,
                        stale: state.summary.stale,
                    };
                    println!("{}", serde_json::to_string(&ev).expect("CheckEvent serialization is infallible"));
                } else {
                    println!("checking {} file(s)...", changed.len());
                    println!(
                        "  errors={} warnings={} checks={}",
                        state.summary.errors,
                        state.summary.warnings,
                        state.summary.checks_run,
                    );
                }

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
