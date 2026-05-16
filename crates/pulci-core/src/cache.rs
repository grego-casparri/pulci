use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone)]
struct CacheEntry {
    mtime: SystemTime,
    size: u64,
}

impl CacheEntry {
    fn from_path(path: &Path) -> Option<Self> {
        let meta = std::fs::metadata(path).ok()?;
        let mtime = meta.modified().ok()?;
        Some(Self {
            mtime,
            size: meta.len(),
        })
    }
}

/// In-memory cache keyed by (mtime, size) to avoid redundant hook runs.
///
/// The OS can fire multiple inotify events for a single save. This cache
/// ensures hooks only run when a file's content has actually changed.
pub struct FileCache {
    entries: HashMap<PathBuf, CacheEntry>,
}

impl Default for FileCache {
    fn default() -> Self {
        Self::new()
    }
}

impl FileCache {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
        }
    }

    /// Returns `true` if the file differs from its last-seen state, and
    /// updates the cache entry. Files not yet seen always return `true`.
    /// Files that cannot be stat-ed (deleted, unreadable) also return `true`
    /// and are evicted from the cache.
    pub fn has_changed(&mut self, path: &Path) -> bool {
        match CacheEntry::from_path(path) {
            None => {
                self.entries.remove(path);
                true
            }
            Some(current) => {
                let changed = self
                    .entries
                    .get(path)
                    .map(|prev| prev.mtime != current.mtime || prev.size != current.size)
                    .unwrap_or(true);

                if changed {
                    self.entries.insert(path.to_path_buf(), current);
                }

                changed
            }
        }
    }

    /// Filters `files`, returning only those whose content has changed.
    pub fn filter_changed(&mut self, files: &[PathBuf]) -> Vec<PathBuf> {
        files
            .iter()
            .filter(|p| self.has_changed(p))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tmp_file(name: &str, content: &[u8]) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .subsec_nanos();
        let path = std::env::temp_dir().join(format!("pulci_cache_{nanos}_{name}"));
        std::fs::write(&path, content).unwrap();
        path
    }

    #[test]
    fn unseen_file_is_changed() {
        let path = tmp_file("a.py", b"x = 1");
        let mut cache = FileCache::new();
        assert!(cache.has_changed(&path));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn unchanged_file_is_not_changed_on_second_call() {
        let path = tmp_file("b.py", b"x = 1");
        let mut cache = FileCache::new();
        cache.has_changed(&path); // prime
        assert!(!cache.has_changed(&path));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn different_size_is_detected() {
        let path = tmp_file("c.py", b"x");
        let mut cache = FileCache::new();
        cache.has_changed(&path); // prime with size=1
        std::fs::write(&path, b"x = 1\ny = 2\n").unwrap(); // size > 1
        assert!(cache.has_changed(&path));
        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_is_changed() {
        let path = PathBuf::from("/tmp/pulci_no_such_file_test.py");
        let mut cache = FileCache::new();
        assert!(cache.has_changed(&path));
    }

    #[test]
    fn filter_changed_returns_only_changed_files() {
        let p1 = tmp_file("d.py", b"a = 1");
        let p2 = tmp_file("e.py", b"b = 2");
        let mut cache = FileCache::new();
        cache.has_changed(&p1); // prime p1, p2 is unseen

        let changed = cache.filter_changed(&[p1.clone(), p2.clone()]);
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0], p2);

        std::fs::remove_file(&p1).ok();
        std::fs::remove_file(&p2).ok();
    }
}
