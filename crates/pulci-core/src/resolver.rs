use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub enum ToolSource {
    Pinned { version: String },
    LocalVenv { path: PathBuf },
    SystemPath { path: PathBuf },
    UvxLatest,
}

#[derive(Debug, Clone)]
pub struct ResolvedTool {
    pub name: &'static str,
    pub version: String,
    pub source: ToolSource,
    /// Full invocation prefix: e.g. `["uvx", "ruff@0.7.4"]` or `["/usr/bin/ruff"]`.
    pub invocation: Vec<String>,
}

impl ResolvedTool {
    /// Convert to the serialisable `ToolInfo` form used in `state.json`. The
    /// `source` enum is flattened to one of the four documented string values
    /// (`pinned` / `local-venv` / `system-path` / `uvx-latest`).
    pub fn to_tool_info(&self) -> crate::state::ToolInfo {
        let (source, path) = match &self.source {
            ToolSource::Pinned { .. } => ("pinned".to_string(), None),
            ToolSource::LocalVenv { path } => (
                "local-venv".to_string(),
                Some(path.display().to_string()),
            ),
            ToolSource::SystemPath { path } => (
                "system-path".to_string(),
                Some(path.display().to_string()),
            ),
            ToolSource::UvxLatest => ("uvx-latest".to_string(), None),
        };
        crate::state::ToolInfo {
            name: self.name.to_owned(),
            version: self.version.clone(),
            source,
            path,
        }
    }
}

/// Resolve `name` using 4-level precedence:
/// 1. Pinned version in pulci.toml → `uvx name@version`
/// 2. Binary in `.venv/bin/` → use that path directly
/// 3. Binary in system PATH → use that path directly
/// 4. uvx fallback → `uvx name` (latest)
///
/// Returns `Err` if no level succeeds with an actionable message.
pub fn resolve_tool(
    name: &'static str,
    project_root: &Path,
    pinned_version: Option<&str>,
) -> anyhow::Result<ResolvedTool> {
    // Level 1: explicit version pin
    if let Some(version) = pinned_version {
        if !uvx_available() {
            anyhow::bail!(
                "`{name}` is pinned to version {version} in pulci.toml but `uvx` is not installed.\n\
                 Install uv: pip install uv"
            );
        }
        let invocation = vec!["uvx".to_owned(), format!("{name}@{version}")];
        // Probe the pinned version at startup so a typo or non-existent version
        // bails with a clear message before the first save, instead of producing
        // a confusing "tool not found" on every hook run.
        let detected = probe_pinned_version(name, version, &invocation)?;
        return Ok(ResolvedTool {
            name,
            version: detected,
            source: ToolSource::Pinned { version: version.to_owned() },
            invocation,
        });
    }

    // Level 2: local venv
    if let Some(path) = find_in_venv(name, project_root) {
        let invocation = vec![path.to_string_lossy().into_owned()];
        let version = detect_version(&invocation);
        return Ok(ResolvedTool {
            name,
            version,
            source: ToolSource::LocalVenv { path },
            invocation,
        });
    }

    // Level 3: system PATH
    if let Some(path) = find_in_system_path(name) {
        let invocation = vec![path.to_string_lossy().into_owned()];
        let version = detect_version(&invocation);
        return Ok(ResolvedTool {
            name,
            version,
            source: ToolSource::SystemPath { path },
            invocation,
        });
    }

    // Level 4: uvx fallback
    if !uvx_available() {
        anyhow::bail!(
            "`{name}` not found in .venv, PATH, or via uvx.\n\
             Install uv (pip install uv) or {name} directly (pip install {name})"
        );
    }
    let invocation = vec!["uvx".to_owned(), name.to_owned()];
    let version = detect_version(&invocation);
    Ok(ResolvedTool {
        name,
        version,
        source: ToolSource::UvxLatest,
        invocation,
    })
}

fn find_in_venv(name: &str, project_root: &Path) -> Option<PathBuf> {
    #[cfg(windows)]
    let candidate = project_root
        .join(".venv")
        .join("Scripts")
        .join(format!("{name}.exe"));
    #[cfg(not(windows))]
    let candidate = project_root.join(".venv").join("bin").join(name);

    candidate.is_file().then_some(candidate)
}

fn find_in_system_path(name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        #[cfg(windows)]
        let candidate = dir.join(format!("{name}.exe"));
        #[cfg(not(windows))]
        let candidate = dir.join(name);

        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn uvx_available() -> bool {
    Command::new("uvx").arg("--version").output().is_ok()
}

/// Run `uvx <name>@<version> --version` and bail with an actionable error
/// if the invocation fails (typo, version not on PyPI, broken cache).
///
/// On success, returns the detected version string. On failure, the error
/// message names the pin, the failing command, and the tool's stderr — so
/// the user can fix `pulci.toml` without guessing what went wrong.
fn probe_pinned_version(
    name: &str,
    version: &str,
    invocation: &[String],
) -> anyhow::Result<String> {
    let (bin, args) = invocation
        .split_first()
        .ok_or_else(|| anyhow::anyhow!("invocation vector is empty"))?;
    let output = Command::new(bin)
        .args(args)
        .arg("--version")
        .output()
        .map_err(|e| {
            anyhow::anyhow!(
                "`{name}` is pinned to version {version} in pulci.toml but `uvx` failed to start: {e}"
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        anyhow::bail!(
            "`{name}` is pinned to version {version} in pulci.toml but uvx \
             could not resolve it.\n\
             Command: {} {} --version\n\
             Stderr: {stderr}\n\
             Check that the version exists on PyPI and that your `pulci.toml [tools]` section is correct.",
            bin,
            args.join(" "),
        );
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().last())
        .unwrap_or("unknown")
        .to_owned())
}

/// Run `invocation --version` and extract the last whitespace-separated token
/// from the first line of stdout. Returns `"unknown"` on any failure.
fn detect_version(invocation: &[String]) -> String {
    let Some((bin, args)) = invocation.split_first() else {
        return "unknown".into();
    };
    let Ok(output) = Command::new(bin).args(args).arg("--version").output() else {
        return "unknown".into();
    };
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .next()
        .and_then(|l| l.split_whitespace().last())
        .unwrap_or("unknown")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir() -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        std::env::temp_dir().join(format!("pulci_resolver_{nanos}"))
    }

    #[test]
    fn find_in_venv_absent_returns_none() {
        let dir = tmp_dir();
        fs::create_dir_all(&dir).unwrap();
        assert!(find_in_venv("ruff", &dir).is_none());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_in_venv_present_returns_path() {
        let dir = tmp_dir();
        #[cfg(windows)]
        let bin_path = dir.join(".venv").join("Scripts").join("ruff.exe");
        #[cfg(not(windows))]
        let bin_path = dir.join(".venv").join("bin").join("ruff");

        fs::create_dir_all(bin_path.parent().unwrap()).unwrap();
        fs::write(&bin_path, b"").unwrap();

        assert_eq!(find_in_venv("ruff", &dir), Some(bin_path));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_in_system_path_nonexistent_tool() {
        // A tool that definitely does not exist anywhere.
        assert!(find_in_system_path("__pulci_nonexistent_binary_xyz__").is_none());
    }

    #[test]
    fn detect_version_bad_command_returns_unknown() {
        let version = detect_version(&["__nonexistent__".to_owned()]);
        assert_eq!(version, "unknown");
    }

    #[test]
    fn resolve_tool_picks_local_venv_over_system_path() {
        let dir = tmp_dir();
        #[cfg(windows)]
        let bin_path = dir.join(".venv").join("Scripts").join("ruff.exe");
        #[cfg(not(windows))]
        let bin_path = dir.join(".venv").join("bin").join("ruff");

        fs::create_dir_all(bin_path.parent().unwrap()).unwrap();
        fs::write(&bin_path, b"").unwrap();

        let resolved = resolve_tool("ruff", &dir, None).unwrap();
        assert!(
            matches!(resolved.source, ToolSource::LocalVenv { .. }),
            "expected LocalVenv, got {:?}",
            resolved.source
        );
        assert_eq!(resolved.name, "ruff");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn resolve_tool_no_venv_falls_back_to_system_or_uvx() {
        let dir = tmp_dir();
        fs::create_dir_all(&dir).unwrap();

        // Tool definitely does not exist; resolve_tool should either find it
        // via uvx (if available) or return an error — never panic.
        let result = resolve_tool("__pulci_nonexistent_xyz__", &dir, None);
        // We don't assert Ok/Err because uvx availability varies in CI.
        // The important guarantee is no panic and a coherent result type.
        let _ = result;
        fs::remove_dir_all(&dir).ok();
    }

    /// Verifies that a pin to a non-existent version bails at startup, not at
    /// first hook run. Network-bound (uvx queries PyPI), so `#[ignore]`'d in CI.
    /// Run manually with: `cargo test -- --ignored pinned_version_that_does_not_exist`.
    #[test]
    #[ignore = "network-bound (uvx → PyPI); run manually with --ignored"]
    fn pinned_version_that_does_not_exist_bails_with_actionable_error() {
        if !uvx_available() {
            return; // can't exercise this path without uvx
        }
        let dir = tmp_dir();
        fs::create_dir_all(&dir).unwrap();

        let result = resolve_tool("ruff", &dir, Some("99.99.99-definitely-not-real"));

        fs::remove_dir_all(&dir).ok();

        let err = result.expect_err("expected resolver to bail on bogus pinned version");
        let msg = err.to_string();
        assert!(msg.contains("99.99.99"), "version not in error: {msg}");
        assert!(msg.contains("ruff"), "tool name not in error: {msg}");
        assert!(msg.contains("pulci.toml"), "config file not mentioned: {msg}");
    }
}
