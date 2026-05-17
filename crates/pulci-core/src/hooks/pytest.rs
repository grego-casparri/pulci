use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::{run_with_timeout, Diagnostic, Hook, Severity};

pub struct PytestAdapter {
    invocation: Vec<String>,
    project_root: PathBuf,
    timeout: Duration,
    /// User-configured glob-like templates with `{stem}` substitution.
    /// Empty falls back to the historical `tests/test_{stem}.py` heuristic.
    test_patterns: Vec<String>,
}

impl PytestAdapter {
    pub fn new(
        resolved: &crate::resolver::ResolvedTool,
        project_root: PathBuf,
        timeout: Duration,
        test_patterns: Vec<String>,
    ) -> Self {
        Self {
            invocation: resolved.invocation.clone(),
            project_root,
            timeout,
            test_patterns,
        }
    }
}

impl Hook for PytestAdapter {
    fn name(&self) -> &'static str {
        "pytest"
    }

    fn run(&self, files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>> {
        let test_files: Vec<PathBuf> = files
            .iter()
            .flat_map(|f| find_test_files(f, &self.test_patterns))
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
        let mut cmd = Command::new(bin);
        cmd.args(prefix_args)
            .args(["--tb=no", "-q", "--no-header"])
            .current_dir(&self.project_root)
            .args(&test_files);
        let output = run_with_timeout(cmd, self.timeout, "pytest")?;

        // pytest exit 0 = all passed, exit 1 = failures found.
        // code() is None when killed by signal — bail rather than silently return empty.
        if output.status.code().is_none() {
            anyhow::bail!("pytest was killed by a signal");
        }

        Ok(parse_pytest_output(&output.stdout))
    }
}

/// Map a source file to its corresponding test file(s).
///
/// Non-Python files yield an empty vec. A source whose stem already starts
/// with `test_` is treated as a test file and returned as-is. Otherwise the
/// configured `test_patterns` are tried in order; each is rendered by
/// substituting `{stem}` with the source's file stem. An empty `test_patterns`
/// falls back to the historical heuristic `tests/test_{stem}.py`.
///
/// Caller filters the returned paths by existence on disk before invoking pytest.
pub(crate) fn find_test_files(source: &Path, test_patterns: &[String]) -> Vec<PathBuf> {
    if source.extension().map(|e| e != "py").unwrap_or(true) {
        return Vec::new();
    }
    let Some(stem) = source.file_stem().and_then(|s| s.to_str()) else {
        return Vec::new();
    };
    if stem.starts_with("test_") {
        return vec![source.to_path_buf()];
    }
    if test_patterns.is_empty() {
        return vec![PathBuf::from("tests").join(format!("test_{stem}.py"))];
    }
    test_patterns
        .iter()
        .map(|p| PathBuf::from(p.replace("{stem}", stem)))
        .collect()
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
    fn find_test_files_default_heuristic_for_source() {
        let result = find_test_files(Path::new("python/pulci/watcher.py"), &[]);
        assert_eq!(result, vec![PathBuf::from("tests/test_watcher.py")]);
    }

    #[test]
    fn find_test_files_returns_source_when_already_a_test_file() {
        let result = find_test_files(Path::new("tests/test_foo.py"), &[]);
        assert_eq!(result, vec![PathBuf::from("tests/test_foo.py")]);
    }

    #[test]
    fn find_test_files_test_self_detection_ignores_patterns() {
        // A file that IS already a test file is returned as-is regardless of
        // what patterns are configured — the user-configured patterns are for
        // source→test mapping, not test→test rewriting.
        let patterns = vec!["different/place/{stem}.py".to_string()];
        let result = find_test_files(Path::new("tests/test_foo.py"), &patterns);
        assert_eq!(result, vec![PathBuf::from("tests/test_foo.py")]);
    }

    #[test]
    fn find_test_files_ignores_rust() {
        assert!(find_test_files(Path::new("src/main.rs"), &[]).is_empty());
    }

    #[test]
    fn find_test_files_ignores_non_python() {
        assert!(find_test_files(Path::new("Makefile"), &[]).is_empty());
    }

    #[test]
    fn find_test_files_substitutes_stem_in_each_pattern() {
        let patterns = vec![
            "tests/test_{stem}.py".to_string(),
            "test/test_{stem}.py".to_string(),
            "tests/unit/test_{stem}.py".to_string(),
        ];
        let result = find_test_files(Path::new("src/foo.py"), &patterns);
        assert_eq!(
            result,
            vec![
                PathBuf::from("tests/test_foo.py"),
                PathBuf::from("test/test_foo.py"),
                PathBuf::from("tests/unit/test_foo.py"),
            ]
        );
    }

    #[test]
    fn find_test_files_pattern_without_stem_placeholder_is_literal() {
        // A template without {stem} produces the same path for every source.
        // Allowed (no error), but caller's existence filter sorts it out.
        let patterns = vec!["tests/conftest.py".to_string()];
        let result = find_test_files(Path::new("src/foo.py"), &patterns);
        assert_eq!(result, vec![PathBuf::from("tests/conftest.py")]);
    }

    #[test]
    fn find_test_files_singular_test_dir_layout() {
        // Regression-style verification for the "tests vs test" gripe.
        let patterns = vec!["test/test_{stem}.py".to_string()];
        let result = find_test_files(Path::new("src/calculator.py"), &patterns);
        assert_eq!(result, vec![PathBuf::from("test/test_calculator.py")]);
    }
}
