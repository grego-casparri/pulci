use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use super::{run_with_timeout, Diagnostic, Hook, Severity};

/// Adapter for `ruff format --check`. Reports files that would be reformatted
/// as `error[ruff_format/format]` diagnostics so consumers can flip them as
/// they would any other lint failure. Companion to `RuffAdapter` (which runs
/// `ruff check`); the two share the resolved `ruff` binary upstream.
pub struct RuffFormatAdapter {
    invocation: Vec<String>,
    timeout: Duration,
}

impl RuffFormatAdapter {
    pub fn new(resolved: &crate::resolver::ResolvedTool, timeout: Duration) -> Self {
        Self {
            invocation: resolved.invocation.clone(),
            timeout,
        }
    }
}

impl Hook for RuffFormatAdapter {
    fn name(&self) -> &'static str {
        "ruff_format"
    }

    fn run(&self, files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>> {
        let (bin, prefix_args) = self
            .invocation
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("ruff invocation vector is empty"))?;
        let mut cmd = Command::new(bin);
        cmd.args(prefix_args)
            .args(["format", "--check"])
            .args(files);
        let output = run_with_timeout(cmd, self.timeout, "ruff_format")?;

        // ruff format --check exits 0 if all files are formatted, 1 if any
        // file would be reformatted, >1 on internal error. None means killed
        // by signal — treat as failure.
        if output.status.code().is_none_or(|c| c > 1) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ruff format failed (exit {}): {stderr}", output.status);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_ruff_format_output(&stdout))
    }
}

/// Parse `ruff format --check` stdout into diagnostics. One diagnostic per
/// `Would reformat: <path>` line. The trailing summary line
/// (`N files would be reformatted, ...`) is ignored. Pure function — testable
/// without subprocess.
pub(crate) fn parse_ruff_format_output(text: &str) -> Vec<Diagnostic> {
    text.lines()
        .filter_map(|line| {
            let path = line.trim().strip_prefix("Would reformat: ")?;
            Some(Diagnostic {
                tool: "ruff_format".into(),
                file: PathBuf::from(path),
                line: 0,
                col: 0,
                severity: Severity::Error,
                code: Some("format".into()),
                message: "would be reformatted by `ruff format`".into(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_WITH_REFORMATS: &str = "\
Would reformat: src/foo.py
Would reformat: src/api/handlers.py
2 files would be reformatted, 5 files already formatted
";

    const SAMPLE_ALL_FORMATTED: &str = "7 files already formatted\n";

    #[test]
    fn parse_two_files_needing_reformat() {
        let diags = parse_ruff_format_output(SAMPLE_WITH_REFORMATS);
        assert_eq!(diags.len(), 2);
        assert_eq!(diags[0].file, PathBuf::from("src/foo.py"));
        assert_eq!(diags[1].file, PathBuf::from("src/api/handlers.py"));
        assert!(diags.iter().all(|d| d.severity == Severity::Error));
        assert!(diags.iter().all(|d| d.code.as_deref() == Some("format")));
        assert!(diags.iter().all(|d| d.tool == "ruff_format"));
    }

    #[test]
    fn parse_all_formatted_returns_empty() {
        let diags = parse_ruff_format_output(SAMPLE_ALL_FORMATTED);
        assert!(diags.is_empty());
    }

    #[test]
    fn parse_empty_output_returns_empty() {
        assert!(parse_ruff_format_output("").is_empty());
    }

    #[test]
    fn summary_line_is_not_misread_as_diagnostic() {
        let only_summary = "3 files would be reformatted, 0 files already formatted\n";
        assert!(parse_ruff_format_output(only_summary).is_empty());
    }

    #[test]
    fn unrelated_lines_are_ignored() {
        let noisy = "\
warning: unexpected message
Would reformat: a.py
some other line
";
        let diags = parse_ruff_format_output(noisy);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, PathBuf::from("a.py"));
    }
}
