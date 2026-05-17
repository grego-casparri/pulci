use std::path::{Path, PathBuf};
use std::sync::mpsc;

use anyhow::Context;
use notify::{recommended_watcher, RecursiveMode, Watcher};

const IGNORED_DIRS: &[&str] = &[
    ".git",
    ".pulci",
    ".ruff_cache",
    ".pytest_cache",
    "__pycache__",
    "node_modules",
    ".venv",
    "target",
];

// Prefix-based ignores for directories whose names include a random suffix
// (e.g. `pytest-cache-files-<hash>` created by pytest's tmp machinery).
const IGNORED_PREFIXES: &[&str] = &["pytest-cache-files-"];

/// Event surfaced from the filesystem watcher to the daemon event loop.
///
/// `Rescan` is emitted when the underlying notify backend reports that some
/// events were lost (e.g. Linux inotify queue overflow). Consumers must treat
/// it as "any file under the watched root may have changed" and re-scan.
#[derive(Debug)]
pub enum FileEvent {
    Changed { path: PathBuf, kind: String },
    Rescan,
}

pub struct WatcherConfig {
    pub path: PathBuf,
}

/// Returns true if any path component matches an ignored directory name or prefix.
pub fn is_ignored(path: &Path) -> bool {
    path.components().any(|c| {
        let name = c.as_os_str();
        IGNORED_DIRS.iter().any(|&d| name == d)
            || name
                .to_str()
                .is_some_and(|s| IGNORED_PREFIXES.iter().any(|&p| s.starts_with(p)))
    })
}

/// Blocks until `tx` is dropped, forwarding non-ignored filesystem events.
/// Sends `()` on `ready_tx` once the filesystem watch is registered.
pub fn watch(
    config: WatcherConfig,
    tx: mpsc::Sender<FileEvent>,
    ready_tx: mpsc::Sender<()>,
) -> anyhow::Result<()> {
    let (notify_tx, notify_rx) = mpsc::channel();

    let mut watcher = recommended_watcher(move |res| {
        let _ = notify_tx.send(res);
    })
    .context("failed to create filesystem watcher")?;

    watcher
        .watch(&config.path, RecursiveMode::Recursive)
        .with_context(|| format!("failed to watch {}", config.path.display()))?;

    let _ = ready_tx.send(());

    loop {
        match notify_rx.recv() {
            Ok(Ok(event)) => {
                // Inotify queue overflow (or equivalent on other backends) surfaces
                // as an Event with Flag::Rescan and empty `paths`. The contract from
                // notify is: assume any file may have changed, refresh in-memory state.
                if event.need_rescan() {
                    if tx.send(FileEvent::Rescan).is_err() {
                        return Ok(());
                    }
                    continue;
                }
                for path in event.paths {
                    if !is_ignored(&path) {
                        let fe = FileEvent::Changed {
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
    fn ignores_pulci_state_dir() {
        // Writing state.json must not trigger a re-check loop.
        assert!(is_ignored(Path::new(".pulci/state.json")));
        assert!(is_ignored(Path::new(".pulci/state.json.tmp")));
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
    fn ignores_ruff_cache() {
        assert!(is_ignored(Path::new(".ruff_cache/0.15.0/abc123")));
    }

    #[test]
    fn ignores_pytest_cache() {
        assert!(is_ignored(Path::new(".pytest_cache/v/cache/nodeids")));
    }

    #[test]
    fn ignores_pytest_cache_files_tmpdir() {
        assert!(is_ignored(Path::new("pytest-cache-files-abc123/README.md")));
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

    /// Verifies that the `notify` crate itself surfaces `Flag::Rescan` when the
    /// kernel queue overflows. Uses a deliberately slow callback (sleep per event)
    /// so notify falls behind faster than the kernel can drain, forcing overflow.
    ///
    /// This is the foundation our fix relies on; if this passes but the
    /// `watcher_emits_rescan_under_inotify_overflow` test fails, the regression
    /// is in our routing of `need_rescan()` to `FileEvent::Rescan`, not in
    /// notify itself.
    #[cfg(unix)]
    #[test]
    #[ignore = "long-running ~30s; uses slow callback to force overflow"]
    fn notify_emits_rescan_flag_with_slow_handler() {
        use notify::{recommended_watcher, RecursiveMode, Watcher};
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let tmp = std::env::temp_dir().join(format!("pulci_notify_repro_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let (tx, rx) = mpsc::channel();
        let mut watcher = recommended_watcher(move |res: notify::Result<notify::Event>| {
            std::thread::sleep(Duration::from_micros(500));
            let _ = tx.send(res);
        })
        .expect("create watcher");
        watcher
            .watch(&tmp, RecursiveMode::Recursive)
            .expect("watch");

        for i in 0..20_000 {
            let p = tmp.join(format!("f{i}.py"));
            let _ = std::fs::write(&p, b"x");
        }

        let mut saw_rescan = false;
        let deadline = Instant::now() + Duration::from_secs(60);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(Ok(event)) if event.need_rescan() => {
                    saw_rescan = true;
                    break;
                }
                Ok(_) => continue,
                Err(_) => continue,
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);
        assert!(
            saw_rescan,
            "notify should emit Rescan when its handler cannot drain the kernel queue"
        );
    }

    /// Reproduction for inotify queue overflow handling.
    ///
    /// On most modern Linux kernels the default queue (16384 events) is hard to
    /// saturate from userspace alone — notify drains it faster than we can write.
    /// To force overflow deterministically, lower the limit first:
    ///
    /// ```text
    /// sudo sysctl -w fs.inotify.max_queued_events=64
    /// cargo test -p pulci-core --release -- --ignored watcher_emits_rescan
    /// sudo sysctl -w fs.inotify.max_queued_events=16384  # restore
    /// ```
    ///
    /// Without that precondition the assertion may fail — the failure message
    /// reports how many Changed events were observed instead, so the diagnosis
    /// is clear ("kernel never overflowed, so the rescan path was not exercised").
    #[cfg(unix)]
    #[test]
    #[ignore = "requires lowered inotify queue limit — see test docstring"]
    fn watcher_emits_rescan_under_inotify_overflow() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};

        let tmp = std::env::temp_dir().join(format!("pulci_overflow_repro_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        let (tx, rx) = mpsc::channel();
        let (ready_tx, ready_rx) = mpsc::channel();

        let watch_path = tmp.clone();
        std::thread::spawn(move || {
            let _ = watch(WatcherConfig { path: watch_path }, tx, ready_tx);
        });

        ready_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("watcher never became ready");

        // Multi-threaded burst — 16 producers × 3000 writes each — to outpace
        // notify's per-iteration drain on as many systems as possible.
        let mut handles = Vec::new();
        for tid in 0..16 {
            let dir = tmp.clone();
            handles.push(std::thread::spawn(move || {
                for i in 0..3_000 {
                    let p = dir.join(format!("t{tid}_f{i}.py"));
                    let _ = std::fs::write(&p, b"x = 1");
                }
            }));
        }
        for h in handles {
            let _ = h.join();
        }

        let mut saw_rescan = false;
        let mut total_changed = 0usize;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            match rx.recv_timeout(Duration::from_millis(500)) {
                Ok(FileEvent::Rescan) => {
                    saw_rescan = true;
                    break;
                }
                Ok(FileEvent::Changed { .. }) => {
                    total_changed += 1;
                }
                Err(_) => continue,
            }
        }

        let _ = std::fs::remove_dir_all(&tmp);

        assert!(
            saw_rescan,
            "expected at least one Rescan event under inotify overflow; observed \
             {total_changed} Changed events. If the kernel queue never overflowed, \
             lower fs.inotify.max_queued_events first (see test docstring).",
        );
    }
}
