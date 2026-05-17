use std::path::{Path, PathBuf};

use crate::watcher::is_ignored;

/// True if `path` is under any of the user-configured `excludes` (relative to `project_root`).
///
/// Used by both the initial project scan and the per-event filter in the
/// daemon loop. Empty `excludes` always returns false.
pub fn is_excluded(path: &Path, project_root: &Path, excludes: &[String]) -> bool {
    excludes
        .iter()
        .any(|excl| path.starts_with(project_root.join(excl)))
}

/// Recursively collect `.py` files under `root`, skipping directories that
/// match the watcher ignore-list and paths under user-configured `excludes`.
///
/// Used by the daemon's initial scan and by the rescan fallback when the
/// watcher reports lost events. Missing or unreadable directories return an
/// empty list rather than erroring — the daemon should keep running even if
/// part of the tree is inaccessible.
pub fn collect_py_files(root: &Path, project_root: &Path, excludes: &[String]) -> Vec<PathBuf> {
    let mut result = Vec::new();
    collect_py_files_inner(root, project_root, excludes, &mut result);
    result
}

fn collect_py_files_inner(
    dir: &Path,
    project_root: &Path,
    excludes: &[String],
    result: &mut Vec<PathBuf>,
) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if is_ignored(&path) {
            continue;
        }
        if is_excluded(&path, project_root, excludes) {
            continue;
        }
        if path.is_dir() {
            collect_py_files_inner(&path, project_root, excludes, result);
        } else if path.extension().map(|e| e == "py").unwrap_or(false) {
            result.push(path);
        }
    }
}

/// Returns true if `path` is a source file that pulci should route to a hook
/// based on its extension. `.py` is always accepted; `.rs` is accepted only
/// when clippy is enabled in `pulci.toml`. Everything else (caches, configs,
/// docs, atomic-write temp files) is rejected.
pub fn is_source_file(path: &Path, clippy_enabled: bool) -> bool {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    ext == "py" || (clippy_enabled && ext == "rs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        std::env::temp_dir().join(format!("pulci_scan_{label}_{nanos}"))
    }

    #[test]
    fn is_excluded_matches_path_under_excluded_dir() {
        let root = PathBuf::from("/project");
        let target = PathBuf::from("/project/benchmarks/fixture/foo.py");
        assert!(is_excluded(
            &target,
            &root,
            &["benchmarks/fixture".to_string()]
        ));
    }

    #[test]
    fn is_excluded_does_not_match_sibling_path() {
        let root = PathBuf::from("/project");
        let target = PathBuf::from("/project/src/foo.py");
        assert!(!is_excluded(
            &target,
            &root,
            &["benchmarks/fixture".to_string()]
        ));
    }

    #[test]
    fn is_excluded_empty_list_excludes_nothing() {
        let root = PathBuf::from("/project");
        let target = PathBuf::from("/project/src/foo.py");
        assert!(!is_excluded(&target, &root, &[]));
    }

    #[test]
    fn is_excluded_matches_multiple_excludes_independently() {
        let root = PathBuf::from("/project");
        let a = PathBuf::from("/project/vendor/x.py");
        let b = PathBuf::from("/project/benchmarks/fixture/y.py");
        let c = PathBuf::from("/project/src/z.py");
        let excludes = vec!["vendor".to_string(), "benchmarks/fixture".to_string()];
        assert!(is_excluded(&a, &root, &excludes));
        assert!(is_excluded(&b, &root, &excludes));
        assert!(!is_excluded(&c, &root, &excludes));
    }

    #[test]
    fn collect_py_files_finds_nested_files() {
        let dir = tmp_dir("nested");
        fs::create_dir_all(dir.join("src").join("sub")).unwrap();
        fs::write(dir.join("a.py"), b"").unwrap();
        fs::write(dir.join("src").join("b.py"), b"").unwrap();
        fs::write(dir.join("src").join("sub").join("c.py"), b"").unwrap();
        fs::write(dir.join("src").join("d.rs"), b"").unwrap();

        let mut files = collect_py_files(&dir, &dir, &[]);
        files.sort();

        assert_eq!(files.len(), 3, "expected 3 .py files, got {files:?}");
        assert!(files.iter().any(|p| p.ends_with("a.py")));
        assert!(files.iter().any(|p| p.ends_with("b.py")));
        assert!(files.iter().any(|p| p.ends_with("c.py")));
        assert!(!files.iter().any(|p| p.extension().is_some_and(|e| e == "rs")));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn collect_py_files_skips_watcher_ignored_dirs() {
        let dir = tmp_dir("ignored");
        fs::create_dir_all(dir.join("__pycache__")).unwrap();
        fs::create_dir_all(dir.join(".venv").join("lib")).unwrap();
        fs::create_dir_all(dir.join("target")).unwrap();
        fs::write(dir.join("a.py"), b"").unwrap();
        fs::write(dir.join("__pycache__").join("cached.py"), b"").unwrap();
        fs::write(dir.join(".venv").join("lib").join("vendor.py"), b"").unwrap();
        fs::write(dir.join("target").join("debug.py"), b"").unwrap();

        let files = collect_py_files(&dir, &dir, &[]);
        assert_eq!(files.len(), 1, "ignored dirs were not skipped: {files:?}");
        assert!(files[0].ends_with("a.py"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn collect_py_files_skips_user_excluded_dirs() {
        let dir = tmp_dir("excluded");
        fs::create_dir_all(dir.join("fixture")).unwrap();
        fs::write(dir.join("a.py"), b"").unwrap();
        fs::write(dir.join("fixture").join("intentional_violations.py"), b"").unwrap();

        let files = collect_py_files(&dir, &dir, &["fixture".to_string()]);
        assert_eq!(files.len(), 1);
        assert!(files[0].ends_with("a.py"));

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn collect_py_files_missing_dir_returns_empty() {
        let dir = tmp_dir("missing").join("does_not_exist");
        let files = collect_py_files(&dir, &dir, &[]);
        assert!(files.is_empty());
    }

    #[test]
    fn collect_py_files_handles_empty_dir() {
        let dir = tmp_dir("empty");
        fs::create_dir_all(&dir).unwrap();
        let files = collect_py_files(&dir, &dir, &[]);
        assert!(files.is_empty());
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn is_source_file_py_always_accepted() {
        assert!(is_source_file(Path::new("foo.py"), false));
        assert!(is_source_file(Path::new("foo.py"), true));
        assert!(is_source_file(Path::new("/abs/path/x.py"), false));
    }

    #[test]
    fn is_source_file_rs_only_when_clippy_enabled() {
        assert!(!is_source_file(Path::new("foo.rs"), false));
        assert!(is_source_file(Path::new("foo.rs"), true));
    }

    #[test]
    fn is_source_file_other_extensions_rejected() {
        assert!(!is_source_file(Path::new("foo.md"), true));
        assert!(!is_source_file(Path::new("foo.toml"), true));
        assert!(!is_source_file(Path::new("Makefile"), true));
        assert!(!is_source_file(Path::new("noextension"), true));
    }

    #[test]
    fn is_source_file_atomic_write_temp_files_rejected() {
        // Regression for commit 4fb6e6a: atomic-write temp files like
        // `foo.py.tmp.12345.abc` must NOT be classified as .py.
        assert!(!is_source_file(Path::new("foo.py.tmp.12345.abc"), true));
        assert!(!is_source_file(Path::new("bar.rs.tmp.9999"), true));
    }
}
