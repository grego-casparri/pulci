use std::path::{Path, PathBuf};
use std::process::Command;

use super::{Diagnostic, Hook, Severity};

pub struct PytestAdapter {
    invocation: Vec<String>,
    project_root: PathBuf,
}

impl PytestAdapter {
    pub fn new(resolved: &crate::resolver::ResolvedTool, project_root: PathBuf) -> Self {
        Self { invocation: resolved.invocation.clone(), project_root }
    }
}

impl Hook for PytestAdapter {
    fn name(&self) -> &'static str {
        "pytest"
    }

    fn run(&self, files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>> {
        let test_files: Vec<PathBuf> = files
            .iter()
            .filter_map(|f| find_test_file(f))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .filter(|p| self.project_root.join(p).exists())
            .collect();

        if test_files.is_empty() {
            return Ok(vec![]);
        }

        let (bin, prefix_args) = self.invocation
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("pytest invocation vector is empty"))?;
        let output = Command::new(bin)
            .args(prefix_args)
            .args(["--tb=no", "-q", "--no-header"])
            .current_dir(&self.project_root)
            .args(&test_files)
            .output()?;

        // pytest exit 0 = all passed, exit 1 = failures found.
        // code() is None when killed by signal — bail rather than silently return empty.
        if output.status.code().is_none() {
            anyhow::bail!("pytest was killed by a signal");
        }

        Ok(parse_pytest_output(&output.stdout))
    }
}

/// Map a source file to its corresponding test file.
///
/// `python/pulci/foo.py` → `tests/test_foo.py`
/// `tests/test_foo.py`   → `tests/test_foo.py` (already a test file)
/// `src/main.rs`         → `None` (non-Python files are ignored)
pub(crate) fn find_test_file(source: &Path) -> Option<PathBuf> {
    if source.extension()? != "py" {
        return None;
    }
    let stem = source.file_stem()?.to_str()?;
    if stem.starts_with("test_") {
        return Some(source.to_path_buf());
    }
    Some(PathBuf::from("tests").join(format!("test_{stem}.py")))
}

/// Parse `pytest --tb=no -q` stdout into diagnostics.
///
/// Recognises lines of the form:
/// ```text
/// FAILED tests/test_foo.py::test_bar - AssertionError: msg
/// ```
pub(crate) fn parse_pytest_output(stdout: &[u8]) -> Vec<Diagnostic> {
    let text = String::from_utf8_lossy(stdout);
    let mut diagnostics = Vec::new();

    for line in text.lines() {
        let Some(rest) = line.trim().strip_prefix("FAILED ") else {
            continue;
        };
        let (test_id, msg) = rest.split_once(" - ").unwrap_or((rest, "test failed"));
        let (file_str, test_name) = test_id.split_once("::").unwrap_or((test_id, "unknown"));

        diagnostics.push(Diagnostic {
            tool: "pytest".into(),
            file: PathBuf::from(file_str),
            line: 0,
            col: 0,
            severity: Severity::Error,
            code: None,
            message: format!("{test_name}: {msg}"),
        });
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &[u8] = b"\
.F.

FAILED tests/test_cli.py::test_version - AssertionError: expected '1.0.0'
FAILED tests/test_watcher.py::test_watch - RuntimeError: timeout

2 failed, 1 passed in 0.45s
";

    #[test]
    fn parse_two_failures() {
        let diags = parse_pytest_output(SAMPLE);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].file, PathBuf::from("tests/test_cli.py"));
        assert!(diags[0].message.contains("test_version"));
        assert_eq!(diags[1].file, PathBuf::from("tests/test_watcher.py"));
        assert_eq!(diags[0].severity, Severity::Error);
    }

    #[test]
    fn parse_clean_run() {
        let diags = parse_pytest_output(b"... 3 passed in 0.12s\n");
        assert!(diags.is_empty());
    }

    #[test]
    fn parse_failure_without_dash_msg() {
        let diags = parse_pytest_output(b"FAILED tests/test_x.py::test_y\n");
        assert_eq!(diags.len(), 1);
        assert!(diags[0].message.contains("test_y"));
    }

    #[test]
    fn find_test_file_for_source() {
        assert_eq!(
            find_test_file(Path::new("python/pulci/watcher.py")),
            Some(PathBuf::from("tests/test_watcher.py"))
        );
    }

    #[test]
    fn find_test_file_for_existing_test() {
        assert_eq!(
            find_test_file(Path::new("tests/test_foo.py")),
            Some(PathBuf::from("tests/test_foo.py"))
        );
    }

    #[test]
    fn find_test_file_ignores_rust() {
        assert!(find_test_file(Path::new("src/main.rs")).is_none());
    }

    #[test]
    fn find_test_file_ignores_non_python() {
        assert!(find_test_file(Path::new("Makefile")).is_none());
    }
}
