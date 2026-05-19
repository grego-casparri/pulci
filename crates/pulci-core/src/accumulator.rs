//! Per-file diagnostic state for pulci, used by `build_state` to publish the
//! aggregated project view as `state.json`. Lives in pulci-core; the daemon
//! in pulci-py instantiates and updates it on each check pass.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::hooks::Diagnostic;
use crate::state::State;

/// In-memory accumulator of per-file diagnostics. An empty `Vec` entry means
/// "this file was checked and is clean"; absence means "never observed or
/// removed". `snapshot()` produces the flattened, sorted diagnostics list
/// that `build_state` writes to `state.json`.
pub struct Accumulator {
    entries: HashMap<PathBuf, Vec<Diagnostic>>,
}

impl Default for Accumulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Accumulator {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Rebuild from a previously-written `State`. Used by the daemon on
    /// startup so state survives restart. Diagnostics in `prev.diagnostics`
    /// are grouped by `d.file`.
    pub fn from_state(prev: &State) -> Self {
        let mut entries: HashMap<PathBuf, Vec<Diagnostic>> = HashMap::new();
        for d in &prev.diagnostics {
            entries.entry(d.file.clone()).or_default().push(d.clone());
        }
        Self { entries }
    }

    /// Set the diagnostics for `path`, replacing any existing entry. Pass an
    /// empty `Vec` to mark the file as known-clean.
    pub fn update(&mut self, path: PathBuf, diagnostics: Vec<Diagnostic>) {
        self.entries.insert(path, diagnostics);
    }

    /// Drop the entry for `path` if present. No-op when absent.
    pub fn remove(&mut self, path: &Path) {
        self.entries.remove(path);
    }

    /// Drop every entry whose path is not present in `current_files`. Used
    /// after the initial scan (to reconcile against offline deletes) and
    /// after a rescan (to reconcile against burst-window deletes).
    pub fn reconcile_with(&mut self, current_files: &HashSet<PathBuf>) {
        self.entries.retain(|path, _| current_files.contains(path));
    }

    /// Flattened, sorted list of every diagnostic in the accumulator. Sorted
    /// by (file, line, col) for stable diffs across writes.
    pub fn snapshot(&self) -> Vec<Diagnostic> {
        let mut out: Vec<Diagnostic> = self
            .entries
            .values()
            .flat_map(|v| v.iter().cloned())
            .collect();
        out.sort_by(|a, b| {
            a.file
                .cmp(&b.file)
                .then(a.line.cmp(&b.line))
                .then(a.col.cmp(&b.col))
        });
        out
    }

    /// Number of files (including clean ones) tracked by the accumulator.
    pub fn files_tracked(&self) -> usize {
        self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::Severity;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn diag(file: &str, line: u32, col: u32, msg: &str) -> Diagnostic {
        Diagnostic {
            tool: "ruff".into(),
            file: PathBuf::from(file),
            line,
            col,
            severity: Severity::Error,
            code: None,
            message: msg.into(),
        }
    }

    fn warn(file: &str, line: u32, msg: &str) -> Diagnostic {
        Diagnostic {
            tool: "ty".into(),
            file: PathBuf::from(file),
            line,
            col: 0,
            severity: Severity::Warning,
            code: None,
            message: msg.into(),
        }
    }

    fn make_state(diagnostics: Vec<Diagnostic>) -> State {
        use crate::state::{State, Summary, SCHEMA_VERSION};
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        State {
            schema_version: SCHEMA_VERSION,
            state_version: 0,
            timestamp: format!("test-{nanos}"),
            summary: Summary { errors: 0, warnings: 0, checks_run: 0, stale: false },
            diagnostics,
            tools: vec![],
            tool_errors: vec![],
        }
    }

    #[test]
    fn new_accumulator_is_empty() {
        let a = Accumulator::new();
        assert_eq!(a.files_tracked(), 0);
        assert!(a.snapshot().is_empty());
    }

    #[test]
    fn files_tracked_counts_keys_including_clean_entries() {
        let mut a = Accumulator::new();
        a.update(PathBuf::from("a.py"), vec![diag("a.py", 1, 0, "x")]);
        a.update(PathBuf::from("b.py"), vec![]); // clean entry counts
        assert_eq!(a.files_tracked(), 2);
    }

    #[test]
    fn snapshot_returns_sorted_flattened_diagnostics() {
        let mut a = Accumulator::new();
        a.update(
            PathBuf::from("b.py"),
            vec![diag("b.py", 5, 0, "second"), diag("b.py", 2, 0, "first")],
        );
        a.update(PathBuf::from("a.py"), vec![diag("a.py", 10, 0, "alpha")]);
        let snap = a.snapshot();
        assert_eq!(snap.len(), 3);
        assert_eq!(snap[0].file, PathBuf::from("a.py"));
        assert_eq!(snap[1].file, PathBuf::from("b.py"));
        assert_eq!(snap[1].line, 2);
        assert_eq!(snap[2].line, 5);
    }

    #[test]
    fn update_adds_entry_with_diagnostics() {
        let mut a = Accumulator::new();
        a.update(PathBuf::from("x.py"), vec![diag("x.py", 1, 0, "msg")]);
        let snap = a.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].message, "msg");
    }

    #[test]
    fn update_with_empty_vec_keeps_entry_as_clean() {
        let mut a = Accumulator::new();
        a.update(PathBuf::from("clean.py"), vec![]);
        assert_eq!(a.files_tracked(), 1);
        assert!(a.snapshot().is_empty());
    }

    #[test]
    fn update_replaces_existing_entry() {
        let mut a = Accumulator::new();
        a.update(PathBuf::from("x.py"), vec![diag("x.py", 1, 0, "old")]);
        a.update(PathBuf::from("x.py"), vec![diag("x.py", 1, 0, "new")]);
        let snap = a.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].message, "new");
    }

    #[test]
    fn remove_deletes_entry() {
        let mut a = Accumulator::new();
        a.update(PathBuf::from("x.py"), vec![diag("x.py", 1, 0, "msg")]);
        a.remove(Path::new("x.py"));
        assert_eq!(a.files_tracked(), 0);
        assert!(a.snapshot().is_empty());
    }

    #[test]
    fn remove_missing_path_is_noop() {
        let mut a = Accumulator::new();
        a.remove(Path::new("nope.py")); // no panic, no error
        assert_eq!(a.files_tracked(), 0);
    }

    #[test]
    fn reconcile_drops_entries_not_in_current_files() {
        let mut a = Accumulator::new();
        a.update(PathBuf::from("keep.py"), vec![diag("keep.py", 1, 0, "k")]);
        a.update(PathBuf::from("drop.py"), vec![diag("drop.py", 1, 0, "d")]);
        let mut current = HashSet::new();
        current.insert(PathBuf::from("keep.py"));
        a.reconcile_with(&current);
        assert_eq!(a.files_tracked(), 1);
        let snap = a.snapshot();
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].file, PathBuf::from("keep.py"));
    }

    #[test]
    fn from_state_rebuilds_groups_diagnostics_by_file() {
        let prev = make_state(vec![
            diag("a.py", 1, 0, "a1"),
            diag("a.py", 2, 0, "a2"),
            warn("b.py", 5, "b_warn"),
        ]);
        let a = Accumulator::from_state(&prev);
        assert_eq!(a.files_tracked(), 2);
        let snap = a.snapshot();
        assert_eq!(snap.len(), 3);
    }

    #[test]
    fn from_state_with_empty_diagnostics_is_empty_accumulator() {
        let prev = make_state(vec![]);
        let a = Accumulator::from_state(&prev);
        assert_eq!(a.files_tracked(), 0);
    }
}
