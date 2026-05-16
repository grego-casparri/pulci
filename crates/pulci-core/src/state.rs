use std::fs;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::hooks::{Diagnostic, Severity};
use crate::orchestrator::RunResult;

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    pub schema_version: u32,
    pub timestamp: String,
    pub summary: Summary,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Summary {
    pub errors: u32,
    pub warnings: u32,
    pub checks_run: u32,
    pub stale: bool,
}

/// Aggregate hook results into a `State` ready to be written to disk.
pub fn build_state(results: &[RunResult]) -> State {
    let mut all_diagnostics: Vec<Diagnostic> = results
        .iter()
        .flat_map(|r| r.diagnostics.iter().cloned())
        .collect();

    // Deterministic ordering: file → line → col.
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

    State {
        schema_version: SCHEMA_VERSION,
        timestamp: now_iso8601(),
        summary: Summary {
            errors,
            warnings,
            checks_run: results.len() as u32,
            stale: false,
        },
        diagnostics: all_diagnostics,
    }
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
fn now_iso8601() -> String {
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
        let state = build_state(&[make_result(2, 1)]);
        assert_eq!(state.summary.errors, 2);
        assert_eq!(state.summary.warnings, 1);
        assert_eq!(state.summary.checks_run, 1);
        assert_eq!(state.schema_version, SCHEMA_VERSION);
        assert!(!state.summary.stale);
    }

    #[test]
    fn build_empty_results() {
        let state = build_state(&[]);
        assert_eq!(state.summary.errors, 0);
        assert_eq!(state.summary.checks_run, 0);
    }

    #[test]
    fn write_and_read_roundtrip() {
        let path = tmp_state_path();
        let state = build_state(&[make_result(1, 0)]);
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
        write_state(&path, &build_state(&[])).unwrap();
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
        let state = build_state(&[results]);
        assert_eq!(state.diagnostics[0].file, PathBuf::from("a.py"));
        assert_eq!(state.diagnostics[1].file, PathBuf::from("b.py"));
    }
}
