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

/// Windows equivalent of the Unix SIGTERM handler: assign the daemon process to
/// a Job Object with `KILL_ON_JOB_CLOSE`. Children spawned by the daemon (ruff,
/// ty, pytest, clippy via uvx) inherit job membership automatically. When the
/// daemon process exits for any reason — clean exit, Ctrl-C, `taskkill`, even
/// `taskkill /F` which Unix SIGTERM cannot intercept — the kernel closes the
/// last handle to the job, which fires KILL_ON_JOB_CLOSE and terminates every
/// still-running child.
///
/// Best-effort: on the rare environment where AssignProcessToJobObject fails
/// (older Windows, restrictive parent job without nested-job support), the
/// daemon logs a warning and continues without child-cleanup guarantee.
#[cfg(windows)]
fn setup_kill_on_close_job() {
    use std::{mem, ptr};
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    unsafe {
        let job = CreateJobObjectW(ptr::null(), ptr::null());
        if job.is_null() {
            eprintln!(
                "pulci: warning: CreateJobObject failed; \
                 child hook subprocesses may outlive the daemon on shutdown"
            );
            return;
        }

        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;

        if SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            mem::size_of_val(&info) as u32,
        ) == 0
        {
            eprintln!(
                "pulci: warning: SetInformationJobObject failed; \
                 child hook subprocesses may outlive the daemon on shutdown"
            );
            CloseHandle(job);
            return;
        }

        if AssignProcessToJobObject(job, GetCurrentProcess()) == 0 {
            eprintln!(
                "pulci: warning: AssignProcessToJobObject failed (already in a non-nestable job?); \
                 child hook subprocesses may outlive the daemon on shutdown"
            );
            CloseHandle(job);
            return;
        }

        // Intentionally do NOT close the job handle. KILL_ON_JOB_CLOSE fires
        // when the last handle to the job closes; keeping ours open ties job
        // lifetime to daemon process lifetime. The kernel reclaims the handle
        // automatically on process exit, which is exactly when we want the
        // termination of in-job children to fire.
    }
}

/// Persist a structured description of a startup-time failure so the MCP
/// server and `pulci status` can surface the cause instead of generic
/// "daemon not running". Best-effort: failures to write the file are
/// swallowed because they would mask the underlying error we are trying
/// to surface (typically a config or tool-resolution issue).
fn write_startup_error(pulci_dir: &Path, error_type: &str, message: &str) {
    if std::fs::create_dir_all(pulci_dir).is_err() {
        return;
    }
    let json = format!(
        "{{\n  \"error_type\": {},\n  \"message\": {},\n  \"timestamp\": {}\n}}\n",
        serde_json::Value::String(error_type.to_string()),
        serde_json::Value::String(message.to_string()),
        serde_json::Value::String(pulci_core::state::now_iso8601()),
    );
    let path = pulci_dir.join("startup_error.json");
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, json).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Clear any prior startup_error file once the daemon has progressed past
/// all fail-fast checks. Without this, a stale error from a previous run
/// would persist and mislead the MCP `not_running` response after the user
/// fixes the underlying issue and re-runs `pulci start`.
fn clear_startup_error(pulci_dir: &Path) {
    let _ = std::fs::remove_file(pulci_dir.join("startup_error.json"));
}

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
use pulci_core::orchestrator::Orchestrator;
use pulci_core::state::{build_state, read_state, write_state};
use pulci_core::scan::{collect_py_files, is_excluded, is_source_file};
use pulci_core::watcher::{watch, FileEvent, WatcherConfig};

/// Returns the pulci-core version string.
#[pyfunction]
fn version() -> &'static str {
    pulci_core::version()
}

/// Group orchestrator results by the file each diagnostic targets. Returns
/// a map keyed by every `path` in `checked_files` — files with no
/// diagnostics get an empty Vec (the "checked clean" signal the accumulator
/// needs).
fn group_results_by_file(
    pass_results: &[pulci_core::orchestrator::RunResult],
    checked_files: &[std::path::PathBuf],
) -> std::collections::HashMap<std::path::PathBuf, Vec<pulci_core::hooks::Diagnostic>> {
    let mut by_file: std::collections::HashMap<
        std::path::PathBuf,
        Vec<pulci_core::hooks::Diagnostic>,
    > = checked_files.iter().map(|p| (p.clone(), Vec::new())).collect();
    for r in pass_results {
        for d in &r.diagnostics {
            by_file.entry(d.file.clone()).or_default().push(d.clone());
        }
    }
    by_file
}

/// Watch `path` for changes, run quality hooks, and write `.pulci/state.json`.
///
/// When `agent` is true, each check event is printed as compiler-style diagnostics
/// instead of human-readable text — suitable for machine consumption.
///
/// Blocks until Ctrl-C (raises KeyboardInterrupt via `py.check_signals()`).
#[pyfunction]
fn start(py: Python<'_>, path: String, agent: bool) -> PyResult<()> {
    // Canonicalize the project root at startup. Without this, a relative
    // CLI arg (`pulci start .`) leaves project_root as the literal "."; later
    // joins (project_root.join(excl)) produce relative paths like "./foo.py"
    // that fail to component-prefix-match the absolute paths notify reports
    // for file events. Symptom: [watch] exclude entries silently no-op.
    // Canonicalize also normalises symlinks and trailing slashes, so the
    // single source of truth for paths is fixed before anything else runs.
    let project_root = PathBuf::from(&path)
        .canonicalize()
        .map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "could not resolve project root {path:?}: {e}"
            ))
        })?;
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

    // Cross-platform shutdown propagation to hook subprocesses.
    //
    // Unix: external `kill -TERM <daemon_pid>` reaches only the daemon, so we
    // register a SIGTERM handler that drains `ACTIVE_HOOK_PIDS` and lets the
    // main loop exit cleanly. Terminal Ctrl-C is unaffected (kernel delivers
    // SIGINT to the whole foreground process group, children die naturally).
    //
    // Windows: there is no SIGTERM. `taskkill` (without /F) sends
    // CTRL_CLOSE_EVENT to console apps but only after a 5s grace; `taskkill /F`
    // is unintercepable. Instead we use a Job Object with KILL_ON_JOB_CLOSE,
    // which lets the kernel terminate every in-job child the moment the
    // daemon's handle to the job closes — covering all exit paths including
    // /F and crashes.
    let shutdown = Arc::new(AtomicBool::new(false));
    #[cfg(unix)]
    {
        use signal_hook::consts::SIGTERM;
        use signal_hook::iterator::Signals;
        let mut signals = Signals::new([SIGTERM]).map_err(|e| {
            pyo3::exceptions::PyRuntimeError::new_err(format!(
                "failed to register SIGTERM handler: {e}"
            ))
        })?;
        let shutdown_clone = Arc::clone(&shutdown);
        std::thread::spawn(move || {
            for _ in signals.forever() {
                shutdown_clone.store(true, Ordering::Relaxed);
                pulci_core::hooks::kill_all_active_hooks();
            }
        });
    }
    #[cfg(windows)]
    setup_kill_on_close_job();

    let config = load_config(&project_root).unwrap_or_else(|e| {
        eprintln!("pulci: warning: failed to load pulci.toml ({e}), using defaults");
        Default::default()
    });

    // Resolve hook subprocess timeout (falls back to DEFAULT_HOOK_TIMEOUT).
    let hook_timeout: Duration = config
        .hooks
        .timeout_secs
        .map(Duration::from_secs)
        .unwrap_or(pulci_core::hooks::DEFAULT_HOOK_TIMEOUT);

    // Build the active hook list via the pulci-core helper. The closure
    // wraps `resolve_tool` with the project_root bound; pulci-core stays
    // PyO3-free and the dispatch logic gets unit-tested without spawning
    // real subprocesses (see hooks::build_hook_list_tests).
    let (hook_list, tool_infos) = pulci_core::hooks::build_hook_list(
        &config.hooks,
        &config.tools,
        &project_root,
        hook_timeout,
        |name, pinned| pulci_core::resolver::resolve_tool(name, &project_root, pinned),
    )
    .map_err(|e| {
        // Persist the cause to `.pulci/startup_error.json` so the MCP server's
        // `pulci_status` call can surface "tool resolution failed" to the
        // agent instead of generic "daemon not running". Without this an
        // agent that ran `pulci start` (which fails fast on bad pins) and
        // then queried the MCP would see `{"status":"not_running"}` with
        // no clue why — and retry indefinitely.
        write_startup_error(&pulci_dir, "tool_resolution_failed", &e.to_string());
        pyo3::exceptions::PyRuntimeError::new_err(e.to_string())
    })?;

    // Foreign-venv warning. When `$PATH` carries a leaked activation from
    // another project, the resolver picks that project's binary instead of
    // a tool actually scoped to this one. Still works, but the user almost
    // certainly didn't mean it. Flag in stderr; doctor surfaces the same.
    for ti in &tool_infos {
        if ti.source == "system-path" {
            if let Some(p) = &ti.path {
                let bin_path = std::path::Path::new(p);
                if !bin_path.starts_with(&project_root) {
                    eprintln!(
                        "pulci: warning: `{}` resolved to {} — outside the project root ({}). \
                         Likely a venv-on-PATH leak from another project. \
                         Install the tool in this project's .venv or pin via [tools] in pulci.toml.",
                        ti.name,
                        p,
                        project_root.display()
                    );
                }
            }
        }
    }

    // Read prior state once: feed both stale detection AND state_version
    // seeding. Without seeding, the monotonic counter resets to 0 on every
    // restart and breaks `pulci_status(since_version=...)` for agents that
    // cached a version from the previous run.
    let prev_state = read_state(&state_file)
        .map_err(|e| eprintln!("pulci: warning: could not read prior state: {e}"))
        .ok();
    if let Some(prev) = &prev_state {
        pulci_core::state::seed_state_version(prev.state_version + 1);
        // Continue the checks_run counter where the previous daemon run
        // left off. Without this the counter resets to 0 every restart,
        // recreating the same "stuck-at-N" confusion an external agent
        // flagged on 0.0.5 (where N was the hook count, but the visual
        // effect is the same — looks like nothing's progressing).
        pulci_core::state::seed_check_passes(prev.summary.checks_run as u64);
    }
    let mut stale = prev_state
        .as_ref()
        .is_some_and(|prev| pulci_core::state::tools_changed(&prev.tools, &tool_infos));

    // Live aggregated diagnostics for the project. Seeded from the prior
    // state.json so a daemon restart keeps the cross-file view; reconciled
    // against the initial scan to drop entries for files deleted offline.
    use pulci_core::accumulator::Accumulator;
    let mut accumulator = match prev_state.as_ref() {
        Some(s) => Accumulator::from_state(s),
        None => Accumulator::new(),
    };

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

    // event_trace::init spawns its writer task on the tokio runtime, so it
    // must run inside the runtime context (otherwise spawn_blocking panics
    // with "no reactor running"). The _guard keeps that context active just
    // long enough to register the task.
    {
        let _guard = rt.enter();
        if let Err(e) = pulci_core::event_trace::init(&pulci_dir, config.debug.event_trace) {
            eprintln!("pulci: event_trace init failed (continuing without trace): {e}");
        }
    }

    let orchestrator = Orchestrator::new(hook_list);
    let mut cache = FileCache::new();

    // Track consecutive write_state failures. After this many in a row the daemon
    // aborts rather than continue serving stale state with a fresh heartbeat —
    // exactly the worst signal an agent could read. Successful write resets to 0.
    const MAX_CONSECUTIVE_WRITE_ERRORS: u32 = 3;
    let mut consecutive_write_errors: u32 = 0;

    // Every fail-fast point above this line writes startup_error.json; we
    // reached this point so all of them passed. Wipe any stale error file
    // from a previous failed run so the MCP server's `not_running` path
    // doesn't surface obsolete state.
    clear_startup_error(&pulci_dir);

    // Initial full-project scan so state.json exists before the first file event.
    {
        let all_py = collect_py_files(&project_root_for_scan, &project_root_for_scan, &config.watch.exclude);
        // Reconcile up front: drop any prev_state entry whose file no longer
        // exists on disk (offline delete between daemon runs). Files that
        // exist but weren't changed keep their prior diagnostics; the
        // changed subset gets overwritten by the orchestrator pass below.
        let initial_files: std::collections::HashSet<std::path::PathBuf> =
            all_py.iter().cloned().collect();
        accumulator.reconcile_with(&initial_files);
        let changed = cache.filter_changed(&all_py);
        if !changed.is_empty() {
            let t0 = Instant::now();
            let results = py.allow_threads(|| rt.block_on(orchestrator.run(&changed)));
            stale = false;

            let by_file = group_results_by_file(&results, &changed);
            for (path, diags) in by_file {
                accumulator.update(path, diags);
            }
            let state = build_state(&accumulator, &results, tool_infos.clone(), stale);

            // Initial sweep on a real project produces 2000+ diagnostics —
            // streaming them line-by-line floods stdout (~370 KB) and fills
            // host pipe buffers for agents that launched pulci as a
            // subprocess. The canonical surface is state.json, queried by
            // `pulci status` or the MCP tool; the stream is auxiliary and
            // not useful when the volume is this high. Suppress in both
            // modes; the footer (printed below) is enough lifecycle signal.
            let elapsed = t0.elapsed().as_secs_f64();
            println!(
                "{} errors, {} warnings ({} files checked, {:.1}s)",
                state.summary.errors,
                state.summary.warnings,
                changed.len(),
                elapsed,
            );
            match write_state(&state_file, &state) {
                Ok(()) => { consecutive_write_errors = 0; }
                Err(e) => {
                    eprintln!("pulci: failed to write initial state: {e}");
                    consecutive_write_errors += 1;
                    if consecutive_write_errors >= MAX_CONSECUTIVE_WRITE_ERRORS {
                        return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                            "pulci aborting: {MAX_CONSECUTIVE_WRITE_ERRORS} consecutive state.json writes failed. Last error: {e}. \
                             Check disk space and .pulci/ permissions."
                        )));
                    }
                }
            }
        } else if prev_state.is_some() {
            // No file content changed since last run, but prev_state existed —
            // possibly with stale entries for files we just reconciled out.
            // Write a fresh state.json reflecting the (possibly trimmed)
            // accumulator so consumers see the live truth on restart.
            let state = build_state(&accumulator, &[], tool_infos.clone(), stale);
            stale = false;
            if let Err(e) = write_state(&state_file, &state) {
                eprintln!("pulci: failed to write post-restart state: {e}");
            }
        }
    }

    loop {
        if shutdown.load(Ordering::Relaxed) {
            if agent {
                println!("{{\"event\":\"stopped\"}}");
            } else {
                println!("Stopped (SIGTERM received).");
            }
            let _ = std::io::stdout().flush();
            break;
        }
        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(first) => {
                let batch_id = pulci_core::event_trace::next_batch_id();
                if let Some(t) = pulci_core::event_trace::tracer() {
                    t.send(pulci_core::event_trace::EventRecord::Debounce {
                        ts_ns: pulci_core::event_trace::ts_ns_now(),
                        action: pulci_core::event_trace::DebounceAction::WindowOpen,
                        batch_id,
                        files: None,
                    });
                }
                let mut paths: HashSet<PathBuf> = HashSet::new();
                let mut removed_paths: HashSet<PathBuf> = HashSet::new();
                let mut needs_rescan = false;
                // Canonicalize every event path at intake. notify does NOT
                // guarantee absolute vs relative consistency across backends
                // or even within a single inotify burst — without this an
                // external agent saw the same file appear twice in
                // `diagnostics[]` once as `/abs/a.py` and once as `a.py`
                // (CONCUR-2 in the 0.0.5 feedback round). Files that fail
                // to canonicalize (deleted between event and intake) are
                // dropped, which is the correct behaviour: a deleted file
                // can't be checked. Falls back to the original path on
                // canonicalize errors that aren't ENOENT.
                let intake = |p: PathBuf, set: &mut HashSet<PathBuf>| {
                    match p.canonicalize() {
                        Ok(canonical) => { set.insert(canonical); }
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                            // File deleted between event and intake; skip.
                        }
                        Err(_) => { set.insert(p); }
                    }
                };
                // Removed events can't canonicalize (file is gone). Try the
                // canonical form for the rename-out case where the parent
                // directory still resolves; fall back to the raw path.
                let intake_removed = |p: PathBuf, set: &mut HashSet<PathBuf>| {
                    let target = p.canonicalize().unwrap_or(p);
                    set.insert(target);
                };
                match first {
                    FileEvent::Rescan => { needs_rescan = true; }
                    FileEvent::Changed { path, .. } => { intake(path, &mut paths); }
                    FileEvent::Removed { path } => { intake_removed(path, &mut removed_paths); }
                }
                let deadline = Instant::now() + Duration::from_millis(50);
                loop {
                    let remaining = deadline.saturating_duration_since(Instant::now());
                    if remaining.is_zero() {
                        break;
                    }
                    match rx.recv_timeout(remaining) {
                        Ok(FileEvent::Rescan) => { needs_rescan = true; }
                        Ok(FileEvent::Changed { path, .. }) => { intake(path, &mut paths); }
                        Ok(FileEvent::Removed { path }) => { intake_removed(path, &mut removed_paths); }
                        Err(_) => break,
                    }
                }
                // If a file was both modified and removed in the same window
                // (rapid edit + delete, or rename-out where the same path
                // appeared via Create then Modify(Name(From))), treat it as
                // removed: the orchestrator can't check a missing file.
                for rp in &removed_paths {
                    paths.remove(rp);
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
                        .filter(|p| is_source_file(p, config.hooks.clippy))
                        .collect()
                };
                if let Some(t) = pulci_core::event_trace::tracer() {
                    t.send(pulci_core::event_trace::EventRecord::Debounce {
                        ts_ns: pulci_core::event_trace::ts_ns_now(),
                        action: pulci_core::event_trace::DebounceAction::WindowClose,
                        batch_id,
                        files: Some(files.clone()),
                    });
                }

                // Apply deletions to the accumulator before anything else so
                // a Removed-only batch still trims state.json.
                for rp in &removed_paths {
                    accumulator.remove(rp);
                }

                let changed = cache.filter_changed(&files);
                if changed.is_empty() {
                    // No checks to run, but if we removed entries (or a
                    // rescan needs reconciliation) we still owe consumers a
                    // fresh state.json.
                    let did_rescan_reconcile = needs_rescan && {
                        let current: std::collections::HashSet<PathBuf> =
                            files.iter().cloned().collect();
                        accumulator.reconcile_with(&current);
                        true
                    };
                    if !removed_paths.is_empty() || did_rescan_reconcile {
                        let state = build_state(&accumulator, &[], tool_infos.clone(), stale);
                        stale = false;
                        match write_state(&state_file, &state) {
                            Ok(()) => { consecutive_write_errors = 0; }
                            Err(e) => {
                                eprintln!("pulci: failed to write state: {e}");
                                consecutive_write_errors += 1;
                                if consecutive_write_errors >= MAX_CONSECUTIVE_WRITE_ERRORS {
                                    return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                                        "pulci aborting: {MAX_CONSECUTIVE_WRITE_ERRORS} consecutive state.json writes failed. Last error: {e}. \
                                         Check disk space and .pulci/ permissions."
                                    )));
                                }
                            }
                        }
                    }
                    continue;
                }

                let t0 = Instant::now();
                let results = py.allow_threads(|| rt.block_on(orchestrator.run(&changed)));

                let by_file = group_results_by_file(&results, &changed);
                for (path, diags) in by_file {
                    accumulator.update(path, diags);
                }
                if needs_rescan {
                    // After a re-collect, trim any accumulator entry whose
                    // path no longer appears in the current file set.
                    let current: std::collections::HashSet<PathBuf> =
                        files.iter().cloned().collect();
                    accumulator.reconcile_with(&current);
                }
                let state = build_state(&accumulator, &results, tool_infos.clone(), stale);
                stale = false; // only first run after tool change is stale
                py.check_signals()?;

                // Per-diagnostic stream is for the interactive human watching
                // the daemon iterate. In agent mode the consumer reads
                // state.json (or the MCP tool) and does not want a flood on
                // stdout — large batches of changed files (e.g. a code-mod
                // across many sources) would otherwise dump thousands of
                // lines. Footer below carries the lifecycle signal in both
                // modes.
                if !agent {
                    for d in &state.diagnostics {
                        let code_part = d.code.as_deref()
                            .map(|c| format!("[{}/{}]", d.tool, c))
                            .unwrap_or_else(|| format!("[{}]", d.tool));
                        println!(
                            "{}:{}:{}: {} {} {}",
                            d.file.display(), d.line, d.col, d.severity, code_part, d.message
                        );
                    }
                }
                let elapsed = t0.elapsed().as_secs_f64();
                println!(
                    "{} errors, {} warnings ({} files checked, {:.1}s)",
                    state.summary.errors,
                    state.summary.warnings,
                    changed.len(),
                    elapsed,
                );

                match write_state(&state_file, &state) {
                    Ok(()) => { consecutive_write_errors = 0; }
                    Err(e) => {
                        eprintln!("pulci: failed to write state: {e}");
                        consecutive_write_errors += 1;
                        if consecutive_write_errors >= MAX_CONSECUTIVE_WRITE_ERRORS {
                            return Err(pyo3::exceptions::PyRuntimeError::new_err(format!(
                                "pulci aborting: {MAX_CONSECUTIVE_WRITE_ERRORS} consecutive state.json writes failed. Last error: {e}. \
                                 Check disk space and .pulci/ permissions."
                            )));
                        }
                    }
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
    // Close the event-trace writer channel before dropping the runtime;
    // otherwise rt.drop() blocks forever on the writer's spawn_blocking task.
    pulci_core::event_trace::shutdown();
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
