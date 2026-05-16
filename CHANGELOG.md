# Changelog

All notable changes to pulci are documented here.
Format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Version scheme: [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
