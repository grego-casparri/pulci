use std::io::ErrorKind;
use std::path::PathBuf;
use std::process::Command;

use super::{Diagnostic, Hook, Severity};

pub struct TyAdapter;

impl Hook for TyAdapter {
    fn name(&self) -> &'static str {
        "ty"
    }

    fn run(&self, files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>> {
        let result = Command::new("ty").arg("check").args(files).output();

        let output = match result {
            Ok(o) => o,
            Err(e) if e.kind() == ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };

        let stdout = String::from_utf8_lossy(&output.stdout);
        Ok(parse_ty_output(&stdout))
    }
}

/// Parse ty's text output into diagnostics. Pure function — testable without subprocess.
///
/// ty emits Rust-compiler-style diagnostics:
/// ```text
/// error[invalid-assignment]: Cannot assign to constant `PI`
///   --> src/main.py:1:1
/// ```
pub(crate) fn parse_ty_output(text: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        let line = lines[i].trim();

        if let Some((severity, rest)) = parse_severity_prefix(line) {
            let (code, message) = parse_code_and_message(rest);

            // Try to read the location from the next `  --> file:line:col` line.
            let loc = lines
                .get(i + 1)
                .and_then(|l| l.trim().strip_prefix("-->"))
                .map(str::trim)
                .and_then(parse_location);

            let has_loc = loc.is_some();
            let (file, ln, col) = loc.unwrap_or((PathBuf::from(""), 0, 0));

            diagnostics.push(Diagnostic {
                tool: "ty".into(),
                file,
                line: ln,
                col,
                severity,
                code,
                message,
            });

            if has_loc {
                i += 2; // consumed the `-->` line
                continue;
            }
        }

        i += 1;
    }

    diagnostics
}

fn parse_severity_prefix(line: &str) -> Option<(Severity, &str)> {
    for (prefix, sev) in [
        ("error", Severity::Error),
        ("warning", Severity::Warning),
        ("info", Severity::Info),
    ] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some((sev, rest));
        }
    }
    None
}

fn parse_code_and_message(s: &str) -> (Option<String>, String) {
    if let Some(inner) = s.strip_prefix('[') {
        if let Some(end) = inner.find(']') {
            let code = inner[..end].to_owned();
            let message = inner[end + 1..].trim_start_matches(':').trim().to_owned();
            return (Some(code), message);
        }
    }
    (None, s.trim_start_matches(':').trim().to_owned())
}

fn parse_location(loc: &str) -> Option<(PathBuf, u32, u32)> {
    // Handles  file:line:col  and  file:line
    let mut parts = loc.splitn(3, ':');
    let file = parts.next().filter(|s| !s.is_empty())?;
    let line: u32 = parts.next()?.trim().parse().ok()?;
    let col: u32 = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    Some((PathBuf::from(file), line, col))
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
error[invalid-assignment]: Cannot assign to constant `PI`
  --> src/main.py:10:5
warning[possibly-unbound]: `x` may be unbound
  --> src/util.py:3:1
";

    #[test]
    fn parse_error_and_warning() {
        let diags = parse_ty_output(SAMPLE);
        assert_eq!(diags.len(), 2);

        assert_eq!(diags[0].severity, Severity::Error);
        assert_eq!(diags[0].code.as_deref(), Some("invalid-assignment"));
        assert_eq!(diags[0].line, 10);
        assert_eq!(diags[0].col, 5);

        assert_eq!(diags[1].severity, Severity::Warning);
        assert_eq!(diags[1].code.as_deref(), Some("possibly-unbound"));
        assert_eq!(diags[1].line, 3);
    }

    #[test]
    fn parse_empty_output() {
        assert!(parse_ty_output("").is_empty());
    }

    #[test]
    fn parse_unrecognised_lines_are_skipped() {
        let diags = parse_ty_output("  | some context line\n  = note: hint\n");
        assert!(diags.is_empty());
    }

    #[test]
    fn parse_no_location() {
        let diags = parse_ty_output("error[foo]: something went wrong\n");
        assert_eq!(diags.len(), 1);
        assert_eq!(diags[0].line, 0);
    }
}
