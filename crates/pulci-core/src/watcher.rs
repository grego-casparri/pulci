use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::Context;
use notify::{recommended_watcher, RecursiveMode, Watcher};
use serde::Serialize;

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".pulci",
    "__pycache__",
    "node_modules",
    ".venv",
    "target",
];

#[derive(Debug, Serialize)]
pub struct FileEvent {
    pub path: PathBuf,
    pub kind: String,
}

pub struct WatcherConfig {
    pub path: PathBuf,
}

/// Returns true if any path component matches an ignored directory name.
pub fn is_ignored(path: &Path) -> bool {
    path.components().any(|c| {
        IGNORED_DIRS
            .iter()
            .any(|&ignored| c.as_os_str() == ignored)
    })
}

/// Blocks until `tx` is dropped, forwarding non-ignored filesystem events.
pub fn watch(config: WatcherConfig, tx: mpsc::Sender<FileEvent>) -> anyhow::Result<()> {
    let (notify_tx, notify_rx) = mpsc::channel();

    let mut watcher = recommended_watcher(move |res| {
        let _ = notify_tx.send(res);
    })
    .context("failed to create filesystem watcher")?;

    watcher
        .watch(&config.path, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", config.path.display()))?;

    loop {
        match notify_rx.recv() {
            Ok(Ok(event)) => {
                for path in event.paths {
                    if !is_ignored(&path) {
                        let fe = FileEvent {
                            path,
                            kind: format!("{:?}", event.kind),
                        };
                        if tx.send(fe).is_err() {
                            return Ok(());
                        }
                    }
                }
            }
            Ok(Err(e)) => return Err(e.into()),
            Err(_) => return Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_git_dir() {
        assert!(is_ignored(Path::new(".git/config")));
    }

    #[test]
    fn ignores_pycache() {
        assert!(is_ignored(Path::new("src/__pycache__/foo.pyc")));
    }

    #[test]
    fn ignores_venv() {
        assert!(is_ignored(Path::new(".venv/lib/python3.11/site.py")));
    }

    #[test]
    fn ignores_node_modules() {
        assert!(is_ignored(Path::new("frontend/node_modules/react/index.js")));
    }

    #[test]
    fn ignores_target() {
        assert!(is_ignored(Path::new("target/debug/pulci")));
    }

    #[test]
    fn allows_src_file() {
        assert!(!is_ignored(Path::new("src/main.rs")));
    }

    #[test]
    fn allows_python_file() {
        assert!(!is_ignored(Path::new("python/pulci/cli.py")));
    }

    #[test]
    fn allows_tests() {
        assert!(!is_ignored(Path::new("tests/test_smoke.py")));
    }
}
