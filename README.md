# pulci

> Continuous quality gate daemon for agent-driven Python development.

[![CI](https://github.com/grego-casparri/pulci/actions/workflows/ci.yml/badge.svg)](https://github.com/grego-casparri/pulci/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Python 3.10+](https://img.shields.io/badge/python-3.10%2B-blue.svg)](https://www.python.org/)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)

**v0.0.1** — Apache-2.0 — [docs/AGENTS.md](docs/AGENTS.md)

## Why

When AI coding agents (Claude Code, Cursor, Codex) iterate on a Python project,
they invoke `ruff check`, `ty check`, `pytest` over and over. Pre-commit hooks
run at commit time. CI runs even later. Nothing in the existing tooling stack was
designed for the loop an agent actually runs: **edit → check → edit, fifty times
an hour**.

**pulci** is a small daemon — Rust core, Python CLI — that runs your configured
quality gates continuously as files change, and exposes the aggregated state as
structured JSON. Agents stop re-invoking tools; they query state.

## Comparison

| Tool          | When it runs        | Output format         | Built for   |
|---------------|---------------------|-----------------------|-------------|
| pre-commit    | Commit time         | Human terminal        | Humans      |
| prek          | Commit time (fast)  | Human terminal        | Humans      |
| MegaLinter    | CI time             | Reports               | CI/CD       |
| pytest-watch  | File change         | Human terminal        | Humans      |
| **pulci**     | **Iteration time**  | **Compiler-style + JSON** | **Agents**  |

pulci does **not** replace any of these. It fills the empty quadrant.

## For AI agents

If you are an AI coding agent, start here: **[docs/AGENTS.md](docs/AGENTS.md)**

The short version: run `pulci start --agent` once, then call `pulci status --json` after each edit instead of invoking ruff/ty/pytest directly.

## Install

```bash
pip install pulci
pulci --version
```

**From source** (to modify the Rust core — requires [Rust stable](https://rustup.rs/) and [uv](https://docs.astral.sh/uv/)):

```bash
git clone https://github.com/grego-casparri/pulci
cd pulci
uv sync
uv run maturin develop --release
uv run pulci --version
```

## Usage

**Start the daemon** (runs in foreground; press Ctrl-C to stop):

```bash
pulci start                   # watches current directory, human output
pulci start /path/to/project  # explicit root
pulci start --agent           # compact JSON events — use this in agent loops
```

**Agent mode output** — compiler-style diagnostics, one per line:

```
src/foo.py:12:1: error[ruff/F401]
  'os' imported but unused
1 error, 0 warnings (3 files checked)
```

**Query current state** (reads `.pulci/state.json`):

```bash
pulci status                        # human-readable table
pulci status --json                 # full JSON for agents
pulci status /path/to/project --json
```

Sample `pulci status --json` output:

```json
{
  "schema_version": 1,
  "timestamp": "2026-05-16T12:00:00Z",
  "summary": { "errors": 2, "warnings": 1, "checks_run": 3, "stale": false },
  "tools": [
    {"name": "ruff", "version": "0.7.4", "source": "local-venv", "path": ".venv/bin/ruff"},
    {"name": "ty",   "version": "0.0.3", "source": "uvx-latest", "path": null}
  ],
  "diagnostics": [
    {
      "tool": "ruff",
      "file": "src/foo.py",
      "line": 12,
      "col": 1,
      "severity": "error",
      "code": "F401",
      "message": "'os' imported but unused"
    }
  ]
}
```

## Configuration

Create `pulci.toml` in the project root (all fields optional):

```toml
[hooks]
ruff   = true    # ruff check on changed .py files (default: true)
ty     = true    # ty check on changed .py files  (default: true)
pytest = false   # pytest on tests/test_<changed>.py (default: false)

[tools]          # optional — pin exact versions for reproducibility
ruff   = "0.7.4" # uses uvx ruff@0.7.4 instead of auto-resolving
ty     = "0.0.3"
```

If `pulci.toml` is absent, defaults apply (`ruff=true`, `ty=true`, `pytest=false`).
If `[tools]` is absent, pulci resolves each tool automatically: `.venv/bin/<tool>` → `$PATH` → `uvx <tool>`.

## Benchmark

A benchmark script is included to compare pulci against manual tool invocation
and prek across N iterations:

```bash
uv run python benchmarks/bench_modes.py --iterations 50
```

Metrics: mean/p50/p95 latency per iteration, total wall time, estimated output
tokens per iteration. pulci's compact `state.json` is a fixed-schema file;
manual tool output grows linearly with the number of violations.

## State file contract

`.pulci/state.json` is the primary contract between pulci and consumers.
Schema version is `1` and will be bumped on breaking changes.

Full schema documented in [`docs/AGENTS.md`](docs/AGENTS.md).

## Roadmap

- [x] File watcher with debounce and ignore filters
- [x] ruff, ty, and pytest hook adapters with parallel execution
- [x] Aggregated JSON state, atomic write, hash-based cache
- [x] `pulci.toml` config, `--agent` output mode
- [x] Benchmark suite
- [ ] PyPI wheels (v0.1)
- [ ] MCP server interface (v0.2)
- [ ] mypy / basedpyright / bandit adapters (v0.2)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[Apache-2.0](LICENSE)
