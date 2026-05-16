use std::path::Path;

use serde::Deserialize;

/// Configuration loaded from `pulci.toml` in the project root.
///
/// All fields have sensible defaults so the file is entirely optional.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    pub hooks: HooksConfig,
}

/// Controls which quality-gate adapters are active.
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Run `ruff check` on changed Python files. Default: true.
    pub ruff: bool,
    /// Run `ty check` on changed Python files. Default: true.
    pub ty: bool,
    /// Run pytest on test files corresponding to changed source files. Default: false.
    pub pytest: bool,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            ruff: true,
            ty: true,
            pytest: false,
        }
    }
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
}
