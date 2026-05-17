# Changelog

All notable changes to pulci are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Version scheme: [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Fixed
- Daemon aborts cleanly after 3 consecutive `state.json` write failures (disk
  full, permissions revoked, FS gone read-only). Previously the daemon
  logged the error and kept running, leaving consumers with a fresh
  heartbeat and stale state — the worst combination because it looks
  healthy. Successful write resets the counter; transient failures don't
  trigger the bail.
- `pulci.toml` pin to a non-existent version (typo or unpublished) now bails at
  daemon startup with a clear message naming the pin, the failing uvx invocation,
  and the tool's stderr. Previously the daemon started cleanly and the user
  saw a confusing "tool not found" on every save until they noticed the typo.
  Implementation: resolver probes `uvx <tool>@<version> --version` for the
  pinned path and propagates the failure as an actionable error.
- Watcher now triggers a full project rescan when `notify` reports an inotify
  queue overflow (`event.need_rescan()`). Previously the overflow event arrived
  as an `Event` with empty `paths` and was silently dropped — the daemon kept
  running while missing every event after the overflow, leaving `state.json`
  stale relative to disk with no signal to consumers. `FileEvent` is now an
  enum (`Changed { path, kind }` / `Rescan`) and the event loop routes `Rescan`
  to the same scan logic used at startup.
- Event loop now filters watcher events to `.py` files (and `.rs` when clippy is
  enabled) before passing them to hooks. Previously, atomic-write temp files
  (e.g. `file.tmp.<pid>.<hash>`) triggered a ruff check that returned E902
  because the temp file was already renamed before ruff opened it.
- `PytestAdapter` now resolves test-file paths relative to the project root and
  sets `Command::current_dir` to the project root, so pytest actually runs when
  the daemon is started from a directory other than the watched project.
- Watcher now ignores `.ruff_cache/`, `.pytest_cache/`, and `pytest-cache-files-*`
  directories. Previously, ruff and pytest writing their caches inside the watched
  tree triggered spurious re-check cycles that overwrote the real state.json.

### Added
- `[watch] exclude` config in `pulci.toml`: paths listed here (relative to the
  project root) are skipped by both the initial scan and all file-change events.
  Useful for fixture or vendor directories that contain intentional violations.
  Example: `exclude = ["benchmarks/fixture"]`.
- Benchmark fixture expanded to 28 files including `tests/test_utils.py`, giving
  the benchmark's steady-state touch target (`sampleapp/utils.py`) a real
  corresponding test file so all three hooks exercise meaningful work.
- Benchmark `_fixture_stats` now correctly counts test failures via `-v` and
  regex, replacing the broken quiet-mode heuristic.

### Added
- `tool_errors` field in `state.json`: aggregates non-diagnostic hook failures
  (timeout, signal kill, parser crash) into a structured array. Closes the
  residual gap from the per-hook timeout work — agents can now distinguish
  "tool ran clean" from "tool never produced a verdict". `pulci status` human
  output gains a "Tool errors" section when non-empty. Field is additive
  (`#[serde(default)]` for backward compat on stale-detection reads).
- Single-instance daemon: `pulci start` acquires an advisory exclusive lock on
  `.pulci/daemon.lock` (`fs2::FileExt::try_lock_exclusive`) before any other I/O.
  A second `pulci start` over the same project root fails fast with an actionable
  message (`"another pulci daemon is already running for this project"`).
- Per-hook timeout: `hooks::run_with_timeout` spawns each hook subprocess, drains
  stdout/stderr in background threads to avoid pipe-full deadlock, and kills the
  child after `DEFAULT_HOOK_TIMEOUT` (120 s). Prevents the daemon from freezing
  when a tool hangs (uvx network stall, deadlocked child, infinite-loop user code
  under pytest). The error is captured by the orchestrator; `state.json` keeps
  advancing rather than staying frozen with a fresh heartbeat.
- Daemon heartbeat: `pulci start` writes `.pulci/heartbeat` every 10 s from a
  background thread, independent of check activity. `pulci status` derives
  `daemon_status` (`alive` / `stale_heartbeat` / `dead`) from the heartbeat age,
  with thresholds 30 s and 120 s. No PID files, no false positives on long checks.
- `pulci status` (human and `--json`) now includes `daemon_status`,
  `daemon_heartbeat_at`, and `age` (`heartbeat_seconds_ago`, `last_check_seconds_ago`).
- `pulci_status` MCP tool returns `{"status": "not_running"}` immediately on the
  blocking path (`since_version`) when the daemon heartbeat is dead, instead of
  waiting for the full timeout.

## [0.0.2] - 2026-05-17

### Added
- `pulci mcp` — MCP server (FastMCP, stdio transport) with `pulci_status` tool.
  Agents call `pulci_status` instead of invoking ruff/ty/pytest directly.
- `pulci mcp info` — prints the JSON config block to paste into
  `claude_desktop_config.json` or `.cursor/mcp.json`. Zero-lookup adoption path.
- `pulci_status` MCP tool accepts `wait_for_file`, `since_version`, and `timeout_ms`
  parameters for causal synchronisation: agents get fresh state after each edit
  with zero polling and zero fixed sleeps (see `docs/AGENTS.md`).
- `state_version` field in `state.json` — monotonic counter incremented on every
  write. Enables the `since_version` wait contract. Schema updated accordingly.
- JSON Schema for `.pulci/state.json` (`schemas/state.v1.schema.json`) and `pulci.toml`
  (`schemas/pulci-toml.schema.json`) — machine-readable contracts for state consumers and
  config authors. Schema links added to `README.md` and `docs/AGENTS.md`.
- `demo/pulci.tape` — reproducible vhs script for the demo GIF. Generate with `vhs demo/pulci.tape`.
- `.prek.yaml`: pre-commit stage (ruff check + ruff format + cargo clippy, ~5 s) and
  pre-push stage (pytest + cargo test, ~30 s) — mirrors CI exactly at commit and push time.
- 4-level tool resolution: pinned version (`pulci.toml [tools]`) → local venv
  (`.venv/bin/`) → system PATH → `uvx` fallback. Zero-config for new projects,
  full determinism for teams that want it.
- `[tools]` config section in `pulci.toml` for explicit version pinning via uvx.
- `tools` field in `state.json` — records which binary was used, its version,
  and its source (`local-venv`, `system-path`, `pinned`, `uvx-latest`).
- `stale` field in `state.json` is now meaningful: `true` when resolved tools
  changed between daemon runs (e.g. ruff updated in venv).
- `pulci status` human output shows a Tools table above diagnostics.

### Changed
- `pulci start --agent` suppresses human-readable startup messages only.
  Diagnostic output is compiler-style in all modes. Structured exit events
  (`{"event":"stopped"}`, `{"event":"error","message":"..."}`) emitted on lifecycle changes.
  Previous per-check JSON event lines (`{"event":"check",...}`) are removed — were aspirational,
  never implemented.
- Diagnostic format in `pulci status` updated to compiler-style:
  `file:line:col: severity[tool/code]`.
- `pulci status` with no daemon running exits 0 (not 1) — missing daemon is not an error.

### Fixed
- Hook adapters now propagate errors instead of panicking or silently returning empty
  results. Signal-killed subprocesses are reported, not swallowed.
- Stale detection logs a warning instead of silently discarding read errors on prior state.
- Watcher no longer triggers re-checks when `.pulci/state.json` itself is written.
- MCP startup messages moved to stderr to avoid corrupting the stdio transport protocol.

## [0.0.1] - 2026-05-16

### Added
- File watcher (`notify` crate) via `pulci start [path]` with ignore filters
  for `.git`, `__pycache__`, `node_modules`, `.venv`, `.pulci`, `target`
- `Hook` trait with adapters for `ruff check`, `ty check`, and `pytest`
- Parallel hook execution via `tokio::task::spawn_blocking` and `JoinSet`
- Hash-based file cache (mtime + size) to skip unchanged files between checks
- Atomic JSON state write to `.pulci/state.json` (write tmp → rename)
- `pulci status` (human) and `pulci status --json` (agent) commands
- `pulci start --agent` mode: compact JSON event lines per check pass
- `pulci.toml` configuration schema (`[hooks]` section: ruff, ty, pytest)
- Selective pytest adapter: `foo.py` → `tests/test_foo.py` heuristic
- Benchmark suite comparing manual tool invocation vs pulci daemon vs prek
- CI on Ubuntu + macOS × Python 3.10–3.13
