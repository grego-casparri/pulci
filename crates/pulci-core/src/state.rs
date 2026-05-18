use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::hooks::{Diagnostic, Severity};
use crate::orchestrator::RunResult;

pub const SCHEMA_VERSION: u32 = 1;

static STATE_VERSION: AtomicU64 = AtomicU64::new(0);
static CHECK_PASS_COUNT: AtomicU64 = AtomicU64::new(0);

/// Return the next monotonic state version. Each call increments the counter.
pub fn next_state_version() -> u64 {
    // Relaxed is sufficient: we only need uniqueness, not ordering relative
    // to other memory operations. The JSON write that follows is the real
    // synchronisation barrier for consumers reading state.json.
    STATE_VERSION.fetch_add(1, Ordering::Relaxed)
}

/// Seed the global counter so the next `next_state_version()` returns `start`.
///
/// Daemon startup reads any prior `state.json` and seeds the counter with
/// `prev.state_version + 1`, so monotonic state_version survives daemon
/// restarts. Without this an agent that cached `since_version=42` before a
/// restart would block on `pulci_status(since_version=42)` until the freshly
/// reset counter caught up — many checks instead of one.
pub fn seed_state_version(start: u64) {
    STATE_VERSION.store(start, Ordering::Relaxed);
}

/// Increment and return the running count of completed check passes. This
/// is what `summary.checks_run` exposes — a monotonic counter of how many
/// times the orchestrator has produced a result set since first daemon
/// run, persisted across restarts via `seed_check_passes`.
pub fn next_check_pass() -> u64 {
    // fetch_add returns the OLD value; first call returns 0 then leaves
    // the counter at 1. We expose `+1` so the first pass reports as
    // checks_run=1 (1-indexed, what a human expects).
    CHECK_PASS_COUNT.fetch_add(1, Ordering::Relaxed) + 1
}

/// Seed the check-pass counter on daemon restart. Mirrors `seed_state_version`:
/// reads the prior `state.json` and continues counting from there so the
/// number a consumer sees keeps climbing across restarts.
pub fn seed_check_passes(start: u64) {
    CHECK_PASS_COUNT.store(start, Ordering::Relaxed);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolInfo {
    pub name: String,
    pub version: String,
    /// "pinned" | "local-venv" | "system-path" | "uvx-latest"
    pub source: String,
    pub path: Option<String>,
}

/// A non-diagnostic failure of a hook (timeout, missing binary, parser crash).
///
/// Surfaces in `state.json` so consumers can distinguish "the tool ran clean"
/// from "the tool never produced a verdict". Diagnostics-as-data; the agent
/// decides whether to retry, abort, or proceed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolError {
    pub tool: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub schema_version: u32,
    /// Monotonically increasing counter incremented on every atomic write.
    /// Agents use this to detect that a new result was produced since their
    /// last read (see D-013: wait_for + since_version synchronisation).
    #[serde(default)]
    pub state_version: u64,
    pub timestamp: String,
    pub summary: Summary,
    pub diagnostics: Vec<Diagnostic>,
    pub tools: Vec<ToolInfo>,
    /// Non-diagnostic hook failures (timeout, signal kill, parser crash).
    /// `#[serde(default)]` so old state.json files without this field still
    /// deserialize cleanly during the stale-detection read on daemon startup.
    #[serde(default)]
    pub tool_errors: Vec<ToolError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub errors: u32,
    pub warnings: u32,
    pub checks_run: u32,
    pub stale: bool,
}

/// Aggregate hook results into a `State` ready to be written to disk.
pub fn build_state(results: &[RunResult], tools: Vec<ToolInfo>, stale: bool) -> State {
    let mut all_diagnostics: Vec<Diagnostic> = results
        .iter()
        .flat_map(|r| r.diagnostics.iter().cloned())
        .collect();

    all_diagnostics.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.line.cmp(&b.line))
            .then(a.col.cmp(&b.col))
    });

    let errors = all_diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Error)
        .count() as u32;
    let warnings = all_diagnostics
        .iter()
        .filter(|d| d.severity == Severity::Warning)
        .count() as u32;

    let mut tool_errors: Vec<ToolError> = results
        .iter()
        .filter_map(|r| {
            r.error.as_ref().map(|msg| ToolError {
                tool: r.tool.clone(),
                message: msg.clone(),
            })
        })
        .collect();
    tool_errors.sort_by(|a, b| a.tool.cmp(&b.tool));

    // `checks_run` historically was `results.len()` — the number of hook
    // adapters active in THIS pass, which on a default config is 2 forever.
    // The name promised a cumulative counter, and an external agent flagged
    // the mismatch in 0.0.5. Reinterpreted in 0.0.6 to actually mean "how
    // many check passes has this daemon produced", monotonic across restarts.
    let checks_run = next_check_pass();

    State {
        schema_version: SCHEMA_VERSION,
        state_version: next_state_version(),
        timestamp: now_iso8601(),
        summary: Summary {
            errors,
            warnings,
            checks_run: checks_run.min(u32::MAX as u64) as u32,
            stale,
        },
        diagnostics: all_diagnostics,
        tools,
        tool_errors,
    }
}

/// Returns true if the resolved tool set changed between daemon runs.
pub fn tools_changed(prev: &[ToolInfo], current: &[ToolInfo]) -> bool {
    if prev.len() != current.len() {
        return true;
    }
    prev.iter()
        .zip(current.iter())
        .any(|(p, c)| p.name != c.name || p.version != c.version || p.source != c.source)
}

/// Atomically write `state` to `state_file` (write tmp → rename).
pub fn write_state(state_file: &Path, state: &State) -> anyhow::Result<()> {
    if let Some(parent) = state_file.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = state_file.with_extension("json.tmp");
    let json = serde_json::to_string_pretty(state)?;
    fs::write(&tmp, &json)?;
    if let Err(e) = fs::rename(&tmp, state_file) {
        let _ = fs::remove_file(&tmp);
        return Err(e.into());
    }
    Ok(())
}

/// Read and deserialise a state file written by `write_state`.
pub fn read_state(state_file: &Path) -> anyhow::Result<State> {
    let bytes = fs::read(state_file)?;
    Ok(serde_json::from_slice(&bytes)?)
}

/// Current UTC time as an ISO 8601 string, without external date dependencies.
pub fn now_iso8601() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    let s = (secs % 60) as u32;
    let m = ((secs / 60) % 60) as u32;
    let h = ((secs / 3600) % 24) as u32;
    let days = (secs / 86400) as u32;
    let (year, month, day) = jdn_to_ymd(days + 2_440_588);

    format!("{year:04}-{month:02}-{day:02}T{h:02}:{m:02}:{s:02}Z")
}

/// Convert a Julian Day Number to a proleptic Gregorian (year, month, day).
fn jdn_to_ymd(jdn: u32) -> (u32, u32, u32) {
    let a = jdn + 32_044;
    let b = (4 * a + 3) / 146_097;
    let c = a - (146_097 * b) / 4;
    let d = (4 * c + 3) / 1_461;
    let e = c - (1_461 * d) / 4;
    let mm = (5 * e + 2) / 153;
    let day = e - (153 * mm + 2) / 5 + 1;
    let month = mm + 3 - 12 * (mm / 10);
    let year = 100 * b + d - 4_800 + mm / 10;
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_state_path() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        std::env::temp_dir()
            .join(format!(".pulci_test_{nanos}"))
            .join("state.json")
    }

    fn make_result(errors: usize, warnings: usize) -> RunResult {
        let mut diags = Vec::new();
        for _ in 0..errors {
            diags.push(Diagnostic {
                tool: "ruff".into(),
                file: PathBuf::from("x.py"),
                line: 1,
                col: 0,
                severity: Severity::Error,
                code: None,
                message: "test error".into(),
            });
        }
        for _ in 0..warnings {
            diags.push(Diagnostic {
                tool: "ty".into(),
                file: PathBuf::from("y.py"),
                line: 2,
                col: 0,
                severity: Severity::Warning,
                code: None,
                message: "test warning".into(),
            });
        }
        RunResult {
            tool: "ruff".into(),
            diagnostics: diags,
            error: None,
        }
    }

    #[test]
    fn build_counts_correctly() {
        // checks_run is a global monotonic counter since 0.0.6 — it depends
        // on the order tests run, so don't assert a specific value. Diagnostic
        // counts still come from the input and ARE deterministic.
        let state = build_state(&[make_result(2, 1)], vec![], false);
        assert_eq!(state.summary.errors, 2);
        assert_eq!(state.summary.warnings, 1);
        assert!(state.summary.checks_run > 0);
        assert_eq!(state.schema_version, SCHEMA_VERSION);
        assert!(!state.summary.stale);
    }

    #[test]
    fn build_empty_results() {
        let state = build_state(&[], vec![], false);
        assert_eq!(state.summary.errors, 0);
        // checks_run increments even when zero hooks ran — it reflects
        // check passes, not hook count.
        assert!(state.summary.checks_run > 0);
    }

    #[test]
    fn checks_run_increases_across_calls() {
        // Lock the counter so parallel test runs don't race.
        let _guard = COUNTER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        seed_check_passes(100);
        let a = build_state(&[], vec![], false).summary.checks_run;
        let b = build_state(&[], vec![], false).summary.checks_run;
        assert_eq!(a, 101);
        assert_eq!(b, 102);
    }

    #[test]
    fn write_and_read_roundtrip() {
        let path = tmp_state_path();
        let state = build_state(&[make_result(1, 0)], vec![], false);
        write_state(&path, &state).unwrap();

        let loaded = read_state(&path).unwrap();
        assert_eq!(loaded.schema_version, SCHEMA_VERSION);
        assert_eq!(loaded.summary.errors, 1);
        assert_eq!(loaded.diagnostics.len(), 1);

        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn write_is_atomic_no_tmp_remains() {
        let path = tmp_state_path();
        write_state(&path, &build_state(&[], vec![], false)).unwrap();
        assert!(path.exists());
        assert!(!path.with_extension("json.tmp").exists());
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn timestamp_is_iso8601() {
        let ts = now_iso8601();
        assert!(ts.contains('T'), "expected T separator, got: {ts}");
        assert!(ts.ends_with('Z'), "expected UTC Z, got: {ts}");
        assert_eq!(ts.len(), 20, "expected 20-char ISO 8601, got: {ts}");
    }

    /// Cross-check `jdn_to_ymd` against an independent ymd→jdn formula on
    /// dates that exercise the corner cases of the Gregorian calendar
    /// (leap year rule + century non-leap rule). Catches "someone flipped
    /// a sign in the algorithm" kind of regressions, which the prior
    /// format-only `timestamp_is_iso8601` test would silently miss.
    #[test]
    fn jdn_to_ymd_round_trips_known_dates_including_leap_rules() {
        // Independent forward conversion (Fliegel & Van Flandern, 1968) —
        // distinct from the inverse in jdn_to_ymd so a shared bug is
        // unlikely.
        fn ymd_to_jdn(year: u32, month: u32, day: u32) -> u32 {
            let a = (14 - month) / 12;
            let y = year + 4800 - a;
            let m = month + 12 * a - 3;
            day + (153 * m + 2) / 5 + 365 * y + y / 4 - y / 100 + y / 400 - 32045
        }
        let cases = [
            (1970, 1, 1),     // Unix epoch
            (2000, 2, 29),    // century year divisible by 400 → IS leap
            (2000, 3, 1),     // day after that leap
            (2024, 2, 29),    // ordinary leap year
            (2024, 12, 31),   // late-in-year normal date
            (2100, 2, 28),    // century year not divisible by 400
            (2100, 3, 1),     // → next day is March 1, not Feb 29 (non-leap)
            (2026, 5, 17),    // current date sanity check
        ];
        for (y, m, d) in cases {
            let jdn = ymd_to_jdn(y, m, d);
            let (ry, rm, rd) = jdn_to_ymd(jdn);
            assert_eq!(
                (ry, rm, rd),
                (y, m, d),
                "jdn_to_ymd round-trip failed for {y}-{m:02}-{d:02} (jdn={jdn})"
            );
        }
    }

    #[test]
    fn diagnostics_sorted_by_file_then_line() {
        let mut results = make_result(0, 0);
        results.diagnostics = vec![
            Diagnostic {
                tool: "ruff".into(),
                file: PathBuf::from("b.py"),
                line: 5,
                col: 0,
                severity: Severity::Error,
                code: None,
                message: "b".into(),
            },
            Diagnostic {
                tool: "ruff".into(),
                file: PathBuf::from("a.py"),
                line: 10,
                col: 0,
                severity: Severity::Error,
                code: None,
                message: "a".into(),
            },
        ];
        let state = build_state(&[results], vec![], false);
        assert_eq!(state.diagnostics[0].file, PathBuf::from("a.py"));
        assert_eq!(state.diagnostics[1].file, PathBuf::from("b.py"));
    }

    #[test]
    fn tools_in_state_roundtrip() {
        let path = tmp_state_path();
        let tools = vec![ToolInfo {
            name: "ruff".into(),
            version: "0.7.4".into(),
            source: "local-venv".into(),
            path: Some(".venv/bin/ruff".into()),
        }];
        let state = build_state(&[], tools, false);
        write_state(&path, &state).unwrap();
        let loaded = read_state(&path).unwrap();
        assert_eq!(loaded.tools.len(), 1);
        assert_eq!(loaded.tools[0].name, "ruff");
        assert_eq!(loaded.tools[0].source, "local-venv");
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn tools_changed_detects_version_bump() {
        let prev = vec![ToolInfo { name: "ruff".into(), version: "0.4.0".into(), source: "local-venv".into(), path: None }];
        let curr = vec![ToolInfo { name: "ruff".into(), version: "0.7.4".into(), source: "local-venv".into(), path: None }];
        assert!(tools_changed(&prev, &curr));
    }

    #[test]
    fn tools_changed_same_returns_false() {
        let tools = vec![ToolInfo { name: "ruff".into(), version: "0.7.4".into(), source: "local-venv".into(), path: None }];
        assert!(!tools_changed(&tools, &tools));
    }

    #[test]
    fn stale_flag_is_persisted() {
        let path = tmp_state_path();
        let state = build_state(&[], vec![], true);
        write_state(&path, &state).unwrap();
        let loaded = read_state(&path).unwrap();
        assert!(loaded.summary.stale);
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }

    #[test]
    fn tool_errors_aggregated_from_failed_results() {
        let r_ok = RunResult {
            tool: "ruff".into(),
            diagnostics: vec![],
            error: None,
        };
        let r_err = RunResult {
            tool: "pytest".into(),
            diagnostics: vec![],
            error: Some("pytest timed out after 120s and was killed by pulci".into()),
        };
        let state = build_state(&[r_ok, r_err], vec![], false);
        assert_eq!(state.tool_errors.len(), 1);
        assert_eq!(state.tool_errors[0].tool, "pytest");
        assert!(state.tool_errors[0].message.contains("timed out"));
        // checks_run is now a global monotonic counter — assert >0 rather
        // than == hooks count (the pre-0.0.6 interpretation).
        assert!(state.summary.checks_run > 0);
    }

    #[test]
    fn tool_errors_empty_when_all_results_ok() {
        let state = build_state(&[make_result(1, 0)], vec![], false);
        assert!(state.tool_errors.is_empty());
    }

    #[test]
    fn tool_errors_sorted_alphabetically_for_stable_diff() {
        let results = vec![
            RunResult {
                tool: "ty".into(),
                diagnostics: vec![],
                error: Some("ty crashed".into()),
            },
            RunResult {
                tool: "clippy".into(),
                diagnostics: vec![],
                error: Some("cargo not found".into()),
            },
        ];
        let state = build_state(&results, vec![], false);
        assert_eq!(state.tool_errors[0].tool, "clippy");
        assert_eq!(state.tool_errors[1].tool, "ty");
    }

    // Tests that mutate STATE_VERSION must serialise. cargo test runs in
    // parallel by default; without this guard the seed test would race the
    // monotonicity test (and any build_state call) for the global counter.
    static COUNTER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn next_state_version_is_monotonically_increasing() {
        let _guard = COUNTER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let a = next_state_version();
        let b = next_state_version();
        assert!(b > a, "expected monotonic increase: a={a} b={b}");
    }

    #[test]
    fn seed_state_version_resets_counter_for_persistence_across_restarts() {
        let _guard = COUNTER_TEST_LOCK
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Simulate a daemon restart: previous run wrote state_version=99, the
        // new daemon seeds the counter to 100 so the next write produces 100.
        seed_state_version(100);
        let v = next_state_version();
        assert_eq!(v, 100, "expected seed to take effect on next call");
        let v2 = next_state_version();
        assert_eq!(v2, 101, "expected continued monotonic increase after seed");
    }

    #[test]
    fn state_without_tool_errors_field_still_deserializes() {
        // Backward compat: a state.json written before tool_errors existed must
        // still load on daemon startup (stale-detection read).
        let path = tmp_state_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let legacy = r#"{
            "schema_version": 1,
            "state_version": 0,
            "timestamp": "2026-05-01T00:00:00Z",
            "summary": {"errors": 0, "warnings": 0, "checks_run": 0, "stale": false},
            "diagnostics": [],
            "tools": []
        }"#;
        fs::write(&path, legacy).unwrap();
        let loaded = read_state(&path).unwrap();
        assert!(loaded.tool_errors.is_empty());
        fs::remove_dir_all(path.parent().unwrap()).ok();
    }
}
