use std::path::PathBuf;
use std::process::Command;

use serde::Deserialize;

use super::{Diagnostic, Hook, Severity};
use crate::resolver::ResolvedTool;

pub struct CargoAdapter {
    invocation: Vec<String>,
}

impl CargoAdapter {
    pub fn new(resolved: &ResolvedTool) -> Self {
        Self { invocation: resolved.invocation.clone() }
    }
}

#[derive(Deserialize)]
struct CargoMessage {
    reason: String,
    message: Option<CompilerMessage>,
}

#[derive(Deserialize)]
struct CompilerMessage {
    level: String,
    message: String,
    code: Option<CompilerCode>,
    spans: Vec<CompilerSpan>,
}

#[derive(Deserialize)]
struct CompilerCode {
    code: String,
}

#[derive(Deserialize)]
struct CompilerSpan {
    file_name: String,
    line_start: u32,
    column_start: u32,
    is_primary: bool,
}

impl Hook for CargoAdapter {
    fn name(&self) -> &'static str {
        "clippy"
    }

    fn run(&self, _files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>> {
        let (bin, prefix_args) = self.invocation
            .split_first()
            .ok_or_else(|| anyhow::anyhow!("cargo invocation vector is empty"))?;
        let output = Command::new(bin)
            .args(prefix_args)
            .args(["clippy", "--workspace", "--message-format=json", "--", "-D", "warnings"])
            .output()?;
        // clippy exits non-zero when warnings/errors are found (-D warnings), so we parse
        // stdout regardless of exit status. A None code (signal kill) is a real failure.
        if output.status.code().is_none() {
            anyhow::bail!("cargo clippy was killed by a signal");
        }
        Ok(parse_clippy_json(&output.stdout))
    }
}

pub(crate) fn parse_clippy_json(stdout: &[u8]) -> Vec<Diagnostic> {
    let text = String::from_utf8_lossy(stdout);
    let mut diagnostics = Vec::new();

    for line in text.lines() {
        let Ok(msg) = serde_json::from_str::<CargoMessage>(line) else {
            continue;
        };
        if msg.reason != "compiler-message" {
            continue;
        }
        let Some(cm) = msg.message else {
            continue;
        };
        let severity = match cm.level.as_str() {
            "error" => Severity::Error,
            "warning" => Severity::Warning,
            _ => continue,
        };
        let Some(span) = cm.spans.iter().find(|s| s.is_primary).or_else(|| cm.spans.first()) else {
            continue;
        };
        diagnostics.push(Diagnostic {
            tool: "clippy".into(),
            file: PathBuf::from(&span.file_name),
            line: span.line_start,
            col: span.column_start,
            severity,
            code: cm.code.map(|c| c.code),
            message: cm.message,
        });
    }

    diagnostics
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_WARNING: &[u8] = br#"{"reason":"compiler-message","package_id":"pulci-core 0.0.1","manifest_path":"/repo/Cargo.toml","target":{"kind":["lib"],"name":"pulci_core"},"message":{"rendered":"warning: unused variable\n","children":[],"code":{"code":"unused_variables","explanation":null},"level":"warning","message":"unused variable: `x`","spans":[{"byte_end":100,"byte_start":96,"column_end":6,"column_start":5,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":5,"line_start":5,"suggested_replacement":null,"suggestion_applicability":null,"text":[]}]}}"#;

    const SAMPLE_ERROR: &[u8] = br#"{"reason":"compiler-message","package_id":"pulci-core 0.0.1","manifest_path":"/repo/Cargo.toml","target":{"kind":["lib"],"name":"pulci_core"},"message":{"rendered":"error[E0308]: mismatched types\n","children":[],"code":{"code":"E0308","explanation":null},"level":"error","message":"mismatched types","spans":[{"byte_end":200,"byte_start":190,"column_end":10,"column_start":5,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":10,"line_start":10,"suggested_replacement":null,"suggestion_applicability":null,"text":[]}]}}"#;

    const NON_MESSAGE_LINE: &[u8] = br#"{"reason":"build-script-executed","package_id":"libc 0.2.0"}"#;

    #[test]
    fn parse_single_warning() {
        let diags = parse_clippy_json(SAMPLE_WARNING);
        assert_eq!(diags.len(), 1);
        let d = &diags[0];
        assert_eq!(d.tool, "clippy");
        assert_eq!(d.severity, Severity::Warning);
        assert_eq!(d.code.as_deref(), Some("unused_variables"));
        assert_eq!(d.line, 5);
        assert_eq!(d.col, 5);
        assert_eq!(d.file, PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn parse_single_error() {
        let diags = parse_clippy_json(SAMPLE_ERROR);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].code.as_deref(), Some("E0308"));
    }

    #[test]
    fn non_message_lines_are_skipped() {
        let diags = parse_clippy_json(NON_MESSAGE_LINE);
        assert!(diags.is_empty());
    }

    #[test]
    fn empty_input_returns_empty() {
        let diags = parse_clippy_json(b"");
        assert!(diags.is_empty());
    }

    #[test]
    fn malformed_json_line_is_skipped() {
        let diags = parse_clippy_json(b"not json at all\n");
        assert!(diags.is_empty());
    }

    #[test]
    fn primary_span_preferred_over_secondary() {
        let json = br#"{"reason":"compiler-message","package_id":"x","manifest_path":"/","target":{"kind":["lib"],"name":"x"},"message":{"rendered":"","children":[],"code":{"code":"E0001","explanation":null},"level":"error","message":"test","spans":[{"byte_end":10,"byte_start":5,"column_end":5,"column_start":1,"expansion":null,"file_name":"secondary.rs","is_primary":false,"label":null,"line_end":2,"line_start":2,"suggested_replacement":null,"suggestion_applicability":null,"text":[]},{"byte_end":20,"byte_start":15,"column_end":8,"column_start":3,"expansion":null,"file_name":"primary.rs","is_primary":true,"label":null,"line_end":7,"line_start":7,"suggested_replacement":null,"suggestion_applicability":null,"text":[]}]}}"#;
        let diags = parse_clippy_json(json);
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].file, PathBuf::from("primary.rs"));
        assert_eq!(diags[0].line, 7);
    }

    #[test]
    fn empty_spans_message_is_skipped() {
        let json = br#"{"reason":"compiler-message","package_id":"x","manifest_path":"/","target":{"kind":["lib"],"name":"x"},"message":{"rendered":"","children":[],"code":null,"level":"warning","message":"no spans","spans":[]}}"#;
        let diags = parse_clippy_json(json);
        assert!(diags.is_empty());
    }

    #[test]
    fn note_level_is_skipped() {
        let note = br#"{"reason":"compiler-message","package_id":"x","manifest_path":"/","target":{"kind":["lib"],"name":"x"},"message":{"rendered":"","children":[],"code":null,"level":"note","message":"a note","spans":[{"byte_end":5,"byte_start":1,"column_end":3,"column_start":1,"expansion":null,"file_name":"src/lib.rs","is_primary":true,"label":null,"line_end":1,"line_start":1,"suggested_replacement":null,"suggestion_applicability":null,"text":[]}]}}"#;
        let diags = parse_clippy_json(note);
        assert!(diags.is_empty());
    }
}
