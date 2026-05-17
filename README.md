# pulci

> Continuous quality gate daemon for agent-driven Python development.

[![CI](https://github.com/grego-casparri/pulci/actions/workflows/ci.yml/badge.svg)](https://github.com/grego-casparri/pulci/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Python 3.10+](https://img.shields.io/badge/python-3.10%2B-blue.svg)](https://www.python.org/)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)

**v0.0.3** — Apache-2.0 — [docs/AGENTS.md](docs/AGENTS.md)

## Why

When AI coding agents (Claude Code, Cursor, Codex) iterate on a Python project,
they invoke `ruff check`, `ty check`, `pytest` over and over. Pre-commit hooks
run at commit time. CI runs even later. Nothing in the existing tooling stack was
designed for the loop an agent actually runs: **edit → check → edit, dozens to
hundreds of times per hour**.

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

## Demo

![pulci watching a project for changes](demo/pulci.gif)

## MCP server

pulci ships an MCP server for Claude Desktop, Cursor, and any MCP-compatible host.

```bash
pulci mcp info          # prints the config block to paste into your host
pulci mcp               # starts the server (stdio)
pulci mcp /path/to/project  # explicit project root
```

`pulci mcp info` output (paste into `claude_desktop_config.json` or `.cursor/mcp.json`):

```json
{
  "mcpServers": {
    "pulci": {
      "command": "/path/to/.venv/bin/pulci",
      "args": ["mcp"]
    }
  }
}
```

Once configured, the host exposes the `pulci_status` tool — agents call it instead of
invoking ruff/ty/pytest directly.

## For AI agents

AI coding agents (Claude Code, Cursor, Codex) should start at **[docs/AGENTS.md](docs/AGENTS.md)**.

The short version: run `pulci start` once, then call `pulci status --json` after each edit instead of invoking ruff/ty/pytest directly.

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
pulci start --agent           # suppress startup messages; structured exit events
```

**Output** — compiler-style diagnostics, one per line (same in both modes):

```
src/foo.py:12:1: error[ruff/F401] 'os' imported but unused
1 error, 0 warnings (3 files checked, 0.4s)
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
  "state_version": 7,
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

- Full schema: [`schemas/state.v1.schema.json`](schemas/state.v1.schema.json)
- Config schema: [`schemas/pulci-toml.schema.json`](schemas/pulci-toml.schema.json)
- Agent documentation: [`docs/AGENTS.md`](docs/AGENTS.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[Apache-2.0](LICENSE)
