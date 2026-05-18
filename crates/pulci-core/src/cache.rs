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
        let prev = self.entries.get(path).cloned();
        let (changed, curr_meta) = match CacheEntry::from_path(path) {
            None => {
                self.entries.remove(path);
                (true, None)
            }
            Some(current) => {
                let changed = prev
                    .as_ref()
                    .map(|p| p.mtime != current.mtime || p.size != current.size)
                    .unwrap_or(true);
                if changed {
                    self.entries.insert(path.to_path_buf(), current.clone());
                }
                (changed, Some(current))
            }
        };

        if let Some(tracer) = crate::event_trace::tracer() {
            let decision = match (prev.is_some(), curr_meta.is_some(), changed) {
                (_, false, _) => crate::event_trace::CacheDecision::Missing,
                (false, true, _) => crate::event_trace::CacheDecision::Unseen,
                (true, true, true) => crate::event_trace::CacheDecision::Changed,
                (true, true, false) => crate::event_trace::CacheDecision::Filtered,
            };
            let to_ns = |s: SystemTime| {
                s.duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_nanos())
            };
            tracer.send(crate::event_trace::EventRecord::Cache {
                ts_ns: crate::event_trace::ts_ns_now(),
                path: path.to_path_buf(),
                decision,
                prev_mtime_ns: prev.as_ref().and_then(|p| to_ns(p.mtime)),
                curr_mtime_ns: curr_meta.as_ref().and_then(|c| to_ns(c.mtime)),
                prev_size: prev.as_ref().map(|p| p.size),
                curr_size: curr_meta.as_ref().map(|c| c.size),
                batch_id: None,
            });
        }

        changed
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
        // Documenta el contrato actual: dos has_changed sin tocar el archivo
        // entre llamadas devuelven false en la segunda. Este es también el
        // smoking gun de Q-17 hipótesis 1: si el OS reporta mtime+size
        // idénticos (NFS, FAT, container con poor mtime res, o burst dentro
        // del mismo ns), el cache filtra el cambio aunque el contenido sea
        // distinto. Cuando llegue el fix de Q-17 hipótesis 1, este test
        // probablemente necesite actualizarse (e.g. agregar hash al criterio).
        let path = tmp_file("b.py", b"x = 1");
        let mut cache = FileCache::new();
        cache.has_changed(&path); // prime
        assert!(!cache.has_changed(&path));
        std::fs::remove_file(&path).ok();
    }

    /// Caso ideal de Q-17 hipótesis 1: reescribir un archivo con contenido
    /// distinto pero mismo `(mtime, size)`. Hoy el cache filtra (= bug). El
    /// fix futuro debería detectar el cambio.
    ///
    /// Marcado `#[ignore]` porque setear mtime determinístico es OS-specific
    /// (libc::utimensat en Unix). Sirve como documentación ejecutable: cuando
    /// alguien quiera reproducir el caso, des-ignore y corre manualmente.
    #[cfg(unix)]
    #[test]
    #[ignore = "documentación de Q-17 hipótesis 1; necesita mtime setter manual"]
    fn same_mtime_same_size_different_content_is_filtered_today() {
        let path = tmp_file("q17_hyp1.py", b"version1");
        let mut cache = FileCache::new();
        cache.has_changed(&path); // prime con (mtime_t0, size=8)

        // Reescribir mismo size pero contenido distinto. Sin tocar mtime,
        // el OS bumpeará — por eso este test es ignored salvo que se setee
        // mtime explícitamente con libc::utimensat antes del segundo check.
        std::fs::write(&path, b"version2").unwrap();

        // Si Q-17 hipótesis 1 está activo y mtime no se bumpea: filtra (bug).
        // Si mtime se bumpea (caso común en filesystems decentes): detecta.
        // En CI no es determinístico → ignored.
        let _ = cache.has_changed(&path);
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
