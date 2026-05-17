# Changelog

All notable changes to pulci are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Version scheme: [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.0.4] - 2026-05-17

Robustness pass plus three user-facing config knobs. Hook subprocesses
no longer outlive the daemon on either Unix or Windows; `state_version`
survives daemon restarts so cached `since_version` values stay valid;
the MCP tool reaches parity with `pulci status --json`. New `[hooks]`
keys make pytest layout, ruff format gating, and per-hook timeout
configurable. One small breaking change: the no-op `wait_for_file`
parameter on the MCP `pulci_status` tool is removed.

### Removed
- `wait_for_file` parameter on the `pulci_status` MCP tool. The parameter
  was documented as a "semantic hint" with no functional effect — the
  daemon produces a single global state, so the actual wait was driven
  entirely by `since_version`. Callers passing it now get a `TypeError`;
  drop the keyword from your calls. Causal synchronisation continues to
  work through `since_version` alone, which is what the code did all along.

### Fixed
- `state_version` now survives daemon restarts. The monotonic counter
  was previously process-local (`AtomicU64::new(0)`), so an agent that
  cached `since_version=42` and then restarted the daemon would block
  on `pulci_status(since_version=42)` until the freshly-reset counter
  caught up. The daemon now seeds the counter from the previous
  `state.json` at startup, so the next emitted version is always
  strictly greater than the last one written — including across
  process boundaries.

### Changed
- MCP `pulci_status` now distinguishes "no daemon" from "daemon alive but
  pre-scan" — matching the CLI behaviour. When `state.json` is absent and
  the heartbeat is alive, the tool returns `{"status": "running_no_checks_yet",
  "daemon_status": "alive", "hint": "..."}` instead of `{"status": "not_running"}`.
- MCP `pulci_status` success responses now carry `daemon_status`,
  `daemon_heartbeat_at`, and `age` (with `heartbeat_seconds_ago` and
  `last_check_seconds_ago`) — the same enrichment `pulci status --json`
  already returns. Agents reading either surface now see the same shape.
  Schema (`state.v1.schema.json`) already declares these as optional fields.
- Internal refactor: extracted heartbeat reading, daemon-health derivation,
  state-file parsing, and response enrichment into `pulci._heartbeat`,
  shared by `pulci.cli` and `pulci.mcp_server`. Narrowed exception
  catches from `except Exception` to specific types (`OSError`,
  `ValueError`, `json.JSONDecodeError`) with one-line stderr warnings so
  failures are debuggable rather than silently swallowed.

### Added
- `[hooks] pytest_test_patterns` in `pulci.toml`: list of templates pulci
  uses to map a changed source file to its test file(s). Each template can
  contain `{stem}`, replaced with the source's file stem. Every existing
  match is fed to pytest. Empty list (default) keeps the historical heuristic
  `tests/test_{stem}.py`. Unblocks projects with non-standard test layouts —
  singular `test/`, nested `tests/unit/`, etc. — without losing the selective
  pytest execution model. Source files that are already test files
  (`test_*.py`) are returned as-is regardless of patterns.
- `[hooks] ruff_format = true` enables a new adapter that runs
  `ruff format --check` on changed `.py` files. Files that would be
  reformatted appear as `error[ruff_format/format]` diagnostics in
  `state.json` and the compiler-style stream, so agents can act on them
  the same way they act on lint errors. Default is `false` (opt-in) —
  format gating is opinionated and many teams prefer to handle it at
  commit time. Independent of the `ruff` hook (lint check): enable one,
  the other, or both. The `ruff` binary is resolved once and shared
  between the two adapters.
- `[hooks] timeout_secs` in `pulci.toml`: per-hook subprocess wall-clock
  timeout, applied uniformly to every enabled hook. Defaults to 120 s when
  omitted. Useful when a project's pytest suite or cold `cargo clippy`
  legitimately exceeds the default. Previously the timeout was a hardcoded
  constant; pinning a longer value avoids spurious `tool_errors` entries
  for slow-but-correct hooks.

### Fixed
- Hook subprocesses (ruff, ty, pytest, clippy) no longer outlive the daemon as
  orphans when an external process terminates `pulci start`. Cross-platform:
  - Unix: SIGTERM handler (via `signal-hook`) drains a global registry of
    active hook PIDs with `libc::kill`, then lets the main loop exit cleanly
    with `{"event":"stopped"}` (agent) or `Stopped (SIGTERM received).` (human)
    and exit code 0. Terminal Ctrl-C was already handled by kernel pgid
    propagation; this is specifically for external `kill <pid>` / systemd /
    supervising scripts where only the daemon receives the signal.
  - Windows: the daemon assigns itself to a Job Object with
    `KILL_ON_JOB_CLOSE` (via `windows-sys`). Children inherit job membership.
    When the daemon exits — clean exit, Ctrl-C, `taskkill`, even `taskkill /F`
    which is unintercepable — the kernel closes the job handle, the flag
    fires, and every still-running child is terminated. Covers more exit
    paths than the Unix mechanism (which cannot intercept SIGKILL).

## [0.0.3] - 2026-05-17

Robustness pass focused on failure modes an agent would notice but a
human might miss: silent staleness, hung hooks, racing daemons, and the
"healthy heartbeat with frozen state" combination that misleads
consumers. New `tool_errors` field surfaces non-diagnostic hook failures
explicitly so absence of diagnostics is no longer ambiguous. Architecture
invariants and public contracts are now documented in `docs/ARCHITECTURE.md`.

### Added
- Single-instance daemon: `pulci start` acquires an advisory exclusive lock on
  `.pulci/daemon.lock` (`fs2::FileExt::try_lock_exclusive`) before any other I/O.
  A second `pulci start` over the same project root fails fast with an actionable
  message (`"another pulci daemon is already running for this project"`).
- Per-hook timeout: `hooks::run_with_timeout` spawns each hook subprocess, drains
  stdout/stderr in background threads to avoid pipe-full deadlock, and kills the
  child after `DEFAULT_HOOK_TIMEOUT` (120 s). Prevents the daemon from freezing
  when a tool hangs (uvx network stall, deadlocked child, infinite-loop user code
  under pytest).
- `tool_errors` field in `state.json`: aggregates non-diagnostic hook failures
  (timeout, signal kill, parser crash) into a structured array. Agents can now
  distinguish "tool ran clean" from "tool never produced a verdict". `pulci status`
  human output gains a "Tool errors" section when non-empty. Field is additive
  (`#[serde(default)]` for backward compat on stale-detection reads).
- Daemon heartbeat: `pulci start` writes `.pulci/heartbeat` every 10 s from a
  background thread, independent of check activity. `pulci status` derives
  `daemon_status` (`alive` / `stale_heartbeat` / `dead`) from the heartbeat age,
  with thresholds 30 s and 120 s. No PID files, no false positives on long checks.
- `pulci status` (human and `--json`) now includes `daemon_status`,
  `daemon_heartbeat_at`, and `age` (`heartbeat_seconds_ago`, `last_check_seconds_ago`).
- `pulci_status` MCP tool returns `{"status": "not_running"}` immediately on the
  blocking path (`since_version`) when the daemon heartbeat is dead, instead of
  waiting for the full timeout.
- `[watch] exclude` config in `pulci.toml`: paths listed here (relative to the
  project root) are skipped by both the initial scan and all file-change events.
  Useful for fixture or vendor directories that contain intentional violations.
  Example: `exclude = ["benchmarks/fixture"]`.
- "Invariants and guarantees" section in `docs/ARCHITECTURE.md` documenting the
  load-bearing properties contributors and integrators must preserve: atomic
  state writes, monotonic `state_version`, single-instance guarantee,
  paralelism constant, debounce window, hook timeout, exit codes contract,
  schema versioning policy, watcher ignore list, and rescan-on-overflow.
- Regression-test policy section in `CONTRIBUTING.md`: every bug fix lands with
  a test that fails pre-fix.
- Benchmark fixture expanded to 28 files including `tests/test_utils.py`, giving
  the benchmark's steady-state touch target (`sampleapp/utils.py`) a real
  corresponding test file so all three hooks exercise meaningful work.

### Changed
- Moved filesystem scan logic (`is_excluded`, `collect_py_files`, and a new
  `is_source_file` helper) from `crates/pulci-py` to `crates/pulci-core::scan`.
  The crate boundary now matches its claim — `pulci-core` is pure Rust with no
  PyO3 dependency, reusable by future non-Python consumers. No behavior change;
  net -32 lines from `pulci-py/src/lib.rs`, +13 Rust unit tests in `pulci-core`
  exercising the scan logic without spinning up Python.

### Fixed
- Watcher now triggers a full project rescan when `notify` reports an inotify
  queue overflow (`event.need_rescan()`). Previously the overflow event arrived
  as an `Event` with empty `paths` and was silently dropped — the daemon kept
  running while missing every event after the overflow, leaving `state.json`
  stale relative to disk with no signal to consumers. `FileEvent` is now an
  enum (`Changed { path, kind }` / `Rescan`) and the event loop routes `Rescan`
  to the same scan logic used at startup.
- Daemon aborts cleanly after 3 consecutive `state.json` write failures (disk
  full, permissions revoked, FS gone read-only). Previously the daemon logged
  the error and kept running, leaving consumers with a fresh heartbeat and stale
  state — the worst combination because it looks healthy. Successful write resets
  the counter; transient failures don't trigger the bail.
- `pulci.toml` pin to a non-existent version (typo or unpublished) now bails at
  daemon startup with a clear message naming the pin, the failing uvx invocation,
  and the tool's stderr. Previously the daemon started cleanly and the user
  saw a confusing "tool not found" on every save until they noticed the typo.
  Implementation: resolver probes `uvx <tool>@<version> --version` for the
  pinned path and propagates the failure as an actionable error.
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
- Benchmark `_fixture_stats` now correctly counts test failures via `-v` and
  regex, replacing the broken quiet-mode heuristic.

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
