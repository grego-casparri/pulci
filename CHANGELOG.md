# Changelog

All notable changes to pulci are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Version scheme: [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
