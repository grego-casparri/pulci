use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use super::{Diagnostic, Hook, Severity};

pub struct RuffAdapter {
    invocation: Vec<String>,
}

impl RuffAdapter {
    pub fn new(resolved: &crate::resolver::ResolvedTool) -> Self {
        Self { invocation: resolved.invocation.clone() }
    }
}

// Serde types matching `ruff check --output-format=json`
#[derive(Deserialize)]
struct RuffEntry {
    code: Option<String>,
    message: String,
    filename: String,
    location: RuffLocation,
}

#[derive(Deserialize)]
struct RuffLocation {
    row: u32,
    column: u32,
}

impl Hook for RuffAdapter {
    fn name(&self) -> &'static str {
        "ruff"
    }

    fn run(&self, files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>> {
        let (bin, prefix_args) = self.invocation
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("ruff invocation vector is empty"))?;
        let output = Command::new(bin)
            .args(prefix_args)
            .args(["check", "--output-format=json"])
            .args(files)
            .output()?;

        // code() is None when the process was killed by a signal — treat as error.
        // ruff exit 0 = clean, exit 1 = violations found, exit > 1 = internal error.
        if output.status.code().is_none_or(|c| c > 1) {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("ruff failed (exit {}): {stderr}", output.status);
        }

        parse_ruff_json(&output.stdout)
    }
}

/// Parse ruff's JSON stdout into diagnostics. Pure function — testable without subprocess.
pub(crate) fn parse_ruff_json(stdout: &[u8]) -> anyhow::Result<Vec<Diagnostic>> {
    let entries: Vec<RuffEntry> = serde_json::from_slice(stdout)
        .map_err(|e| anyhow::anyhow!("failed to parse ruff JSON output: {e}"))?;
    Ok(entries
        .into_iter()
        .map(|e| Diagnostic {
            tool: "ruff".into(),
            file: PathBuf::from(e.filename),
            line: e.location.row,
            col: e.location.column,
            severity: Severity::Error,
            code: e.code,
            message: e.message,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_JSON: &[u8] = br#"[
        {
            "code": "F401",
            "message": "`os` imported but unused",
            "filename": "src/main.py",
            "location": {"row": 1, "column": 0},
            "end_location": {"row": 1, "column": 9},
            "fix": null,
            "url": "https://docs.astral.sh/ruff/rules/unused-import",
            "noqa_row": 1
        }
    ]"#;

    #[test]
    fn parse_single_violation() {
        let diags = parse_ruff_json(SAMPLE_JSON).unwrap();
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.tool, "ruff");
        assert_eq!(d.code.as_deref(), Some("F401"));
        assert_eq!(d.line, 1);
        assert_eq!(d.col, 0);
        assert_eq!(d.severity, Severity::Error);
        assert!(d.message.contains("unused"));
    }

    #[test]
    fn parse_empty_array() {
        let diags = parse_ruff_json(b"[]").unwrap();
        assert!(diags.is_empty());
    }

    #[test]
    fn parse_malformed_json_returns_error() {
        assert!(parse_ruff_json(b"not json").is_err());
    }
}
