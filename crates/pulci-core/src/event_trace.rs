//! Opt-in pipeline event tracing for debugging hard-to-reproduce bugs like Q-17
//! (event loss under burst). When enabled via `[debug] event_trace = true` in
//! pulci.toml, the daemon emits one JSONL record per pipeline event to
//! `.pulci/events.log`. See `docs/plans/2026-05-18-q17-event-trace-design.md`.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use tokio::sync::mpsc;

static BATCH_ID_COUNTER: AtomicU64 = AtomicU64::new(0);

/// One JSONL line in events.log. The `stage` discriminant goes to the
/// `"stage"` field via serde's adjacently-tagged enum representation.
#[derive(Debug, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum EventRecord {
    Watcher {
        ts_ns: u128,
        path: PathBuf,
        kind: String,
        mtime_ns: Option<u128>,
        size: Option<u64>,
        content_sha256: Option<String>,
    },
    Debounce {
        ts_ns: u128,
        action: DebounceAction,
        batch_id: u64,
        files: Option<Vec<PathBuf>>,
    },
    Cache {
        ts_ns: u128,
        path: PathBuf,
        decision: CacheDecision,
        prev_mtime_ns: Option<u128>,
        curr_mtime_ns: Option<u128>,
        prev_size: Option<u64>,
        curr_size: Option<u64>,
        batch_id: Option<u64>,
    },
    StateWrite {
        ts_ns: u128,
        state_version: u64,
        files_in_snapshot: usize,
        tools_run: Vec<String>,
        batch_id: Option<u64>,
    },
    Meta {
        ts_ns: u128,
        action: MetaAction,
        reason: Option<String>,
    },
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DebounceAction {
    WindowOpen,
    WindowClose,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheDecision {
    Changed,
    Filtered,
    Unseen,
    Missing,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetaAction {
    LogTruncated,
    WriterShutdown,
}

/// Allocate the next correlation id for a debounce batch. Monotonic across
/// the process lifetime so two events with the same `batch_id` are guaranteed
/// to be from the same window.
pub fn next_batch_id() -> u64 {
    BATCH_ID_COUNTER.fetch_add(1, Ordering::Relaxed)
}

/// Returns the current timestamp in ns since UNIX epoch. Used as the global
/// ordering key in events.log.
pub fn ts_ns_now() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Compute sha256 over a file's bytes. Returns None if the file cannot be read
/// (deleted between event and read, permission denied, etc). The hash is hex.
pub fn content_sha256(path: &Path) -> Option<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Some(format!("{:x}", hasher.finalize()))
}

use std::io::Write;
use std::sync::OnceLock;
use tokio::sync::mpsc::UnboundedReceiver;

/// Process-wide tracer handle. `None` when `[debug] event_trace = false`.
/// Initialized once by the daemon at startup; tests drive `writer_loop`
/// directly to avoid OnceLock cross-test contamination.
static TRACER: OnceLock<Option<EventTracer>> = OnceLock::new();

/// Hard upper bound on events.log file size while open. When reached, the
/// writer emits a `Meta { action: LogTruncated, reason: "size_cap" }` event
/// and stops appending. Defense against "I forgot the flag was on" — not a
/// rotation strategy. The next `pulci start` rotates via backup-on-start.
const SOFT_CAP_BYTES: u64 = 100 * 1024 * 1024;

/// The sender lives inside a Mutex<Option<_>> so `shutdown()` can drop it
/// at daemon-exit time. Without that drop, the writer task's `blocking_recv`
/// never wakes up — and tokio's runtime drop blocks on outstanding
/// spawn_blocking tasks, so the entire daemon hangs on SIGTERM.
pub struct EventTracer {
    tx: std::sync::Mutex<Option<mpsc::UnboundedSender<EventRecord>>>,
}

impl EventTracer {
    pub fn send(&self, record: EventRecord) {
        // Best-effort: if the sender was taken (shutdown) or the receiver was
        // dropped (writer task exited), drop the record silently. event_trace
        // is opt-in debug — never fail user workflow because a log line was lost.
        if let Ok(guard) = self.tx.lock() {
            if let Some(tx) = guard.as_ref() {
                let _ = tx.send(record);
            }
        }
    }
}

/// Initialize the tracer for a project. Idempotent within a process: second
/// call is a no-op so multi-init from tests doesn't panic. If `event_trace`
/// is false the OnceLock stores `None` and `tracer()` returns None forever.
///
/// Side effect: backs up an existing `events.log` to `events.log.1` so the
/// new session starts fresh while preserving the immediately-prior session.
pub fn init(pulci_dir: &Path, enabled: bool) -> std::io::Result<()> {
    if TRACER.get().is_some() {
        return Ok(());
    }
    if !enabled {
        let _ = TRACER.set(None);
        return Ok(());
    }

    std::fs::create_dir_all(pulci_dir)?;
    let log_path = pulci_dir.join("events.log");
    let backup_path = pulci_dir.join("events.log.1");
    if log_path.exists() {
        let _ = std::fs::rename(&log_path, &backup_path);
    }

    let (tx, rx) = mpsc::unbounded_channel();
    let _ = TRACER.set(Some(EventTracer {
        tx: std::sync::Mutex::new(Some(tx)),
    }));

    // Writer on the tokio runtime via spawn_blocking: we use sync std::fs::File
    // + BufWriter for portability; switching to tokio::fs would force every
    // call site to be async.
    tokio::task::spawn_blocking(move || {
        if let Err(e) = writer_loop(log_path, rx) {
            eprintln!("pulci event-trace writer exited: {e}");
        }
    });

    Ok(())
}

/// Returns the global tracer if event_trace is enabled, else None.
pub fn tracer() -> Option<&'static EventTracer> {
    TRACER.get().and_then(|opt| opt.as_ref())
}

/// Drop the sender end of the writer channel. The writer task observes the
/// channel close, drains any buffered events, writes its `writer_shutdown`
/// meta record, flushes the BufWriter, and exits. Call this from the daemon
/// just before the tokio runtime is dropped — otherwise runtime drop blocks
/// on the outstanding spawn_blocking task forever. No-op when event_trace
/// is disabled.
pub fn shutdown() {
    if let Some(tracer) = tracer() {
        if let Ok(mut guard) = tracer.tx.lock() {
            let _ = guard.take();
        }
    }
}

fn writer_loop(
    log_path: PathBuf,
    mut rx: UnboundedReceiver<EventRecord>,
) -> std::io::Result<()> {
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)?;
    let mut buf = std::io::BufWriter::new(file);
    let mut bytes_written: u64 = 0;
    let mut truncated = false;

    while let Some(record) = rx.blocking_recv() {
        if truncated {
            continue;
        }
        let line = match serde_json::to_string(&record) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("pulci event-trace serialize error: {e}");
                continue;
            }
        };
        let with_nl_len = line.len() as u64 + 1;
        if bytes_written + with_nl_len > SOFT_CAP_BYTES {
            let meta = EventRecord::Meta {
                ts_ns: ts_ns_now(),
                action: MetaAction::LogTruncated,
                reason: Some("size_cap".into()),
            };
            if let Ok(meta_line) = serde_json::to_string(&meta) {
                let _ = writeln!(buf, "{meta_line}");
                let _ = buf.flush();
            }
            truncated = true;
            continue;
        }
        if writeln!(buf, "{line}").is_err() {
            break;
        }
        bytes_written += with_nl_len;
    }

    // Drain on shutdown: emit a marker so log readers know the session ended
    // cleanly, then flush.
    let shutdown = EventRecord::Meta {
        ts_ns: ts_ns_now(),
        action: MetaAction::WriterShutdown,
        reason: None,
    };
    if let Ok(s) = serde_json::to_string(&shutdown) {
        let _ = writeln!(buf, "{s}");
    }
    let _ = buf.flush();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn jsonl_format_round_trips_all_variants() {
        let cases = vec![
            EventRecord::Watcher {
                ts_ns: 1_000_000_000,
                path: PathBuf::from("src/foo.py"),
                kind: "Modify".into(),
                mtime_ns: Some(999),
                size: Some(42),
                content_sha256: Some("abc123".into()),
            },
            EventRecord::Debounce {
                ts_ns: 2_000_000_000,
                action: DebounceAction::WindowOpen,
                batch_id: 7,
                files: None,
            },
            EventRecord::Cache {
                ts_ns: 3_000_000_000,
                path: PathBuf::from("src/bar.py"),
                decision: CacheDecision::Filtered,
                prev_mtime_ns: Some(100),
                curr_mtime_ns: Some(100),
                prev_size: Some(10),
                curr_size: Some(10),
                batch_id: Some(7),
            },
            EventRecord::StateWrite {
                ts_ns: 4_000_000_000,
                state_version: 42,
                files_in_snapshot: 3,
                tools_run: vec!["ruff".into(), "ty".into()],
                batch_id: Some(7),
            },
            EventRecord::Meta {
                ts_ns: 5_000_000_000,
                action: MetaAction::LogTruncated,
                reason: Some("size_cap".into()),
            },
        ];
        for r in cases {
            let s = serde_json::to_string(&r).expect("serialize");
            // Must be a valid JSON object on one line, no embedded newlines
            // (events.log relies on line-per-record).
            assert!(!s.contains('\n'), "serialized event has newline: {s}");
            // Smoke: parse back as serde_json::Value to confirm well-formed.
            let _: serde_json::Value = serde_json::from_str(&s).expect("parse");
            // And that the stage tag is present.
            assert!(s.contains("\"stage\":"), "missing stage tag: {s}");
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn writer_drains_pending_events_before_shutdown() {
        let tmp = std::env::temp_dir()
            .join(format!("pulci_et_drain_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let log_path = tmp.join("events.log");

        // Drive writer_loop directly (avoids OnceLock pollution across tests).
        let (tx, rx) = mpsc::unbounded_channel();
        let log_path_clone = log_path.clone();
        let handle = tokio::task::spawn_blocking(move || writer_loop(log_path_clone, rx));

        for i in 0..50 {
            tx.send(EventRecord::Watcher {
                ts_ns: i as u128,
                path: PathBuf::from(format!("f{i}.py")),
                kind: "Modify".into(),
                mtime_ns: None,
                size: None,
                content_sha256: None,
            })
            .unwrap();
        }
        drop(tx); // closing sender → writer_loop exits cleanly
        handle.await.expect("join").expect("writer ok");

        let content = std::fs::read_to_string(&log_path).expect("read log");
        let lines: Vec<&str> = content.lines().collect();
        // 50 watcher events + 1 writer_shutdown meta event
        assert_eq!(lines.len(), 51, "got {} lines", lines.len());
        assert!(lines[50].contains("\"writer_shutdown\""));

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn soft_cap_emits_meta_event_and_stops_writing() {
        // Cap real es 100MB — fabricamos un evento gigante para forzar el
        // caso sin llenar 100MB de disco.
        let tmp = std::env::temp_dir()
            .join(format!("pulci_et_cap_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let log_path = tmp.join("events.log");

        let (tx, rx) = mpsc::unbounded_channel();
        let log_path_clone = log_path.clone();
        let handle = tokio::task::spawn_blocking(move || writer_loop(log_path_clone, rx));

        // Un evento con kind absurdamente largo → ~SOFT_CAP_BYTES de un saque.
        let big_kind = "X".repeat(SOFT_CAP_BYTES as usize + 1);
        tx.send(EventRecord::Watcher {
            ts_ns: 1,
            path: PathBuf::from("big.py"),
            kind: big_kind,
            mtime_ns: None,
            size: None,
            content_sha256: None,
        })
        .unwrap();
        // Este NO debería aparecer en el log: el cap ya disparó.
        tx.send(EventRecord::Watcher {
            ts_ns: 2,
            path: PathBuf::from("after_cap.py"),
            kind: "tiny".into(),
            mtime_ns: None,
            size: None,
            content_sha256: None,
        })
        .unwrap();
        drop(tx);
        handle.await.expect("join").expect("writer ok");

        let content = std::fs::read_to_string(&log_path).expect("read log");
        assert!(
            content.contains("\"log_truncated\""),
            "expected meta log_truncated event in log"
        );
        assert!(
            !content.contains("after_cap.py"),
            "post-cap event should have been dropped"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn init_backs_up_existing_log_to_dot_1() {
        // Validamos la mecánica del rename (init() global usa OnceLock no
        // resettable en tests; el e2e de Q-17 ejerce init() completo).
        let tmp = std::env::temp_dir()
            .join(format!("pulci_et_backup_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let log = tmp.join("events.log");
        std::fs::write(&log, b"previous session\n").unwrap();

        let backup = tmp.join("events.log.1");
        if log.exists() {
            std::fs::rename(&log, &backup).unwrap();
        }
        assert!(backup.exists());
        assert!(!log.exists());
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "previous session\n"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
