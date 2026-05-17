use std::path::Path;

use serde::Deserialize;

/// Configuration loaded from `pulci.toml` in the project root.
///
/// All fields have sensible defaults so the file is entirely optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hooks: HooksConfig,
    pub tools: ToolsConfig,
    pub watch: WatchConfig,
}

/// Controls which paths the daemon watches and scans.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct WatchConfig {
    /// Relative paths (from project root) to skip entirely — initial scan and file events.
    /// Useful for fixture or vendor directories that contain intentional violations.
    pub exclude: Vec<String>,
}

/// Controls which quality-gate adapters are active.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Run `ruff check` on changed Python files. Default: true.
    pub ruff: bool,
    /// Run `ruff format --check` on changed Python files. Default: false.
    /// Independent of `ruff` — enable one, the other, or both.
    pub ruff_format: bool,
    /// Run `ty check` on changed Python files. Default: true.
    pub ty: bool,
    /// Run pytest on test files corresponding to changed source files. Default: false.
    pub pytest: bool,
    /// Run `cargo clippy --workspace` on any Rust file change. Default: false.
    pub clippy: bool,
    /// Per-hook subprocess wall-clock timeout in seconds. Applies uniformly to
    /// every enabled hook. `None` falls back to `DEFAULT_HOOK_TIMEOUT` (120 s).
    /// Useful when a real test suite legitimately runs longer than the default.
    pub timeout_secs: Option<u64>,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            ruff: true,
            ruff_format: false,
            ty: true,
            pytest: false,
            clippy: false,
            timeout_secs: None,
        }
    }
}

/// Controls which version of each tool pulci uses (level-1 pinning).
///
/// All fields are `Option<String>`. `None` means "auto-resolve via
/// venv / PATH / uvx fallback". A value pins to that version via uvx.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    pub ruff: Option<String>,
    pub ty: Option<String>,
    pub pytest: Option<String>,
}

/// Load `pulci.toml` from `project_root`, using defaults if the file is absent.
pub fn load_config(project_root: &Path) -> anyhow::Result<Config> {
    let path = project_root.join("pulci.toml");
    if !path.exists() {
        return Ok(Config::default());
    }
    let raw = std::fs::read_to_string(&path)?;
    let config: Config = toml::from_str(&raw)?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn write_toml(content: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let dir = std::env::temp_dir().join(format!("pulci_cfg_test_{nanos}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pulci.toml"), content).unwrap();
        dir
    }

    #[test]
    fn defaults_when_no_file() {
        let dir = std::env::temp_dir().join("pulci_cfg_absent");
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = load_config(&dir).unwrap();
        assert!(cfg.hooks.ruff);
        assert!(cfg.hooks.ty);
        assert!(!cfg.hooks.pytest);
    }

    #[test]
    fn explicit_values_override_defaults() {
        let dir = write_toml("[hooks]\nruff = false\npytest = true\n");
        let cfg = load_config(&dir).unwrap();
        assert!(!cfg.hooks.ruff);
        assert!(cfg.hooks.ty); // default
        assert!(cfg.hooks.pytest);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn empty_file_uses_all_defaults() {
        let dir = write_toml("");
        let cfg = load_config(&dir).unwrap();
        assert!(cfg.hooks.ruff);
        assert!(cfg.hooks.ty);
        assert!(!cfg.hooks.pytest);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn partial_hooks_section() {
        let dir = write_toml("[hooks]\nty = false\n");
        let cfg = load_config(&dir).unwrap();
        assert!(cfg.hooks.ruff); // default
        assert!(!cfg.hooks.ty);
        assert!(!cfg.hooks.pytest); // default
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tools_version_parsed() {
        let dir = write_toml("[tools]\nruff = \"0.7.4\"\n");
        let cfg = load_config(&dir).unwrap();
        assert_eq!(cfg.tools.ruff.as_deref(), Some("0.7.4"));
        assert!(cfg.tools.ty.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clippy_defaults_to_false() {
        let dir = write_toml("");
        let cfg = load_config(&dir).unwrap();
        assert!(!cfg.hooks.clippy);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn clippy_enabled_via_toml() {
        let dir = write_toml("[hooks]\nclipy = false\nruff = true\n");
        let cfg = load_config(&dir).unwrap();
        assert!(cfg.hooks.ruff);
        assert!(!cfg.hooks.clippy);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tools_section_absent_gives_none() {
        let dir = write_toml("[hooks]\nruff = true\n");
        let cfg = load_config(&dir).unwrap();
        assert!(cfg.tools.ruff.is_none());
        assert!(cfg.tools.ty.is_none());
        assert!(cfg.tools.pytest.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watch_exclude_parsed() {
        let dir = write_toml("[watch]\nexclude = [\"benchmarks/fixture\", \"vendor\"]\n");
        let cfg = load_config(&dir).unwrap();
        assert_eq!(cfg.watch.exclude, vec!["benchmarks/fixture", "vendor"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn watch_exclude_defaults_empty() {
        let dir = write_toml("");
        let cfg = load_config(&dir).unwrap();
        assert!(cfg.watch.exclude.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn timeout_secs_defaults_to_none() {
        let dir = write_toml("");
        let cfg = load_config(&dir).unwrap();
        assert!(cfg.hooks.timeout_secs.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn timeout_secs_parsed_when_present() {
        let dir = write_toml("[hooks]\ntimeout_secs = 300\n");
        let cfg = load_config(&dir).unwrap();
        assert_eq!(cfg.hooks.timeout_secs, Some(300));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ruff_format_defaults_to_false() {
        let dir = write_toml("");
        let cfg = load_config(&dir).unwrap();
        assert!(!cfg.hooks.ruff_format);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ruff_format_independent_of_ruff() {
        let dir = write_toml("[hooks]\nruff = false\nruff_format = true\n");
        let cfg = load_config(&dir).unwrap();
        assert!(!cfg.hooks.ruff);
        assert!(cfg.hooks.ruff_format);
        std::fs::remove_dir_all(&dir).ok();
    }
}
