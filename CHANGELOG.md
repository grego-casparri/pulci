# Changelog

All notable changes to pulci are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Version scheme: [Semantic Versioning](https://semver.org/).

## [Unreleased]

### Added
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
- `pulci start --agent` now emits compiler-style diagnostics (aligns with
  `FORMATS.md` / D-007). Previous JSON event lines (`{"event":"check",...}`)
  are removed. NDJSON event mode deferred to v0.2 (`--events` flag).
- Diagnostic format in `pulci status` updated to compiler-style:
  `file:line:col: severity[tool/code]`.

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
