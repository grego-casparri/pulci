# pulci

> Continuous quality gate daemon for agent-driven Python development.

[![CI](https://github.com/grego-casparri/pulci/actions/workflows/ci.yml/badge.svg)](https://github.com/grego-casparri/pulci/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue.svg)](LICENSE)
[![Python 3.10+](https://img.shields.io/badge/python-3.10%2B-blue.svg)](https://www.python.org/)
[![Rust](https://img.shields.io/badge/rust-stable-orange.svg)](https://www.rust-lang.org/)

**v0.0.6** — Apache-2.0 — [docs/AGENTS.md](docs/AGENTS.md)

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

pulci does **not** replace any of these. Each row runs in a different slot — pulci is the one that runs while you are editing.

## Demo

![pulci watching a project for changes](demo/pulci.gif)

## MCP server

pulci ships an MCP server for Claude Desktop, Cursor, and any MCP-compatible host.

**Auto-install** (recommended) — writes the entry into the host's config file directly, preserving any other MCP servers you have:

```bash
pulci mcp install claude-desktop      # ~/Library/Application Support/Claude/...
pulci mcp install cursor              # .cursor/mcp.json (project-local)
pulci mcp install cursor --global     # ~/.cursor/mcp.json (all projects)
pulci mcp install cursor --dry-run    # preview without writing
```

For Claude Code, use its native `claude mcp add pulci $(which pulci) mcp` instead.

**Manual setup** — `pulci mcp info` prints the JSON block you can paste yourself:

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

## Real example: what the agent loop looks like

I built pulci because I wanted my own Claude Code sessions to stop wasting tokens re-invoking ruff after every edit. Here is the actual loop:

**Before pulci** — each edit pays the cold start of every tool, the agent re-parses the output, and the diagnostic stream grows linearly with violation count:

```text
agent: edit src/api.py
agent: $ ruff check src/api.py
        ... 2391 tokens of JSON ...
agent: $ ty check src/api.py
        ... more tokens ...
agent: parse, decide next action
```

**With pulci** — the daemon already ran the checks in the background. The agent reads `state_version` before the edit, waits for it to advance after, then reads a single structured response:

```python
# 1. Capture the version before editing
v = (await pulci_status())["state_version"]

# 2. Make the edit (write_file, replace_in_file, whatever)
edit_file("src/api.py", "...")

# 3. Wait for the daemon to process it — no polling, no fixed sleeps
result = await pulci_status(since_version=v)

# 4. Branch on the structured response
if result.get("tool_errors"):
    # A hook timed out or crashed — retry or surface
    ...
elif result["summary"]["errors"] > 0:
    # Real diagnostics; iterate
    for d in result["diagnostics"]:
        ...
else:
    # Clean. Move on.
```

**Benchmark (0.0.7, fixture: 28 Python files, 50 iterations, ruff + ty + pytest):**

| Mode | mean ms | p95 ms | tokens/call |
|---|---:|---:|---:|
| manual (`ruff` + `ty` + `pytest` per iteration) | 469 | 542 | 2,397 |
| **pulci** (daemon warm, agent reads `state.json`) | **397** | 555 | 2,312 |
| prek (`prek run --all-files` per iteration) | 630 | 1,058 | 2,491 |

Single-edit latency: pulci is ~15% faster than direct invocation and ~1.6× faster than prek. Token cost is similar across modes because pulci's `state.json` now carries the *live aggregated project view* (every file's current diagnostics, not just the file you touched) — agents that ask "is the project clean?" get the real answer in one read instead of running tools again.

**Where the savings really show up is burst editing** (5 files edited in quick succession, simulating an agent applying a multi-file refactor):

| Mode | tokens/burst | mean s/burst |
|---|---:|---:|
| manual (5 separate tool invocations) | 12,040 | 2.81 |
| **pulci** (one daemon read for the whole batch) | **2,337** | **0.52** |

That is a **5.2× token reduction and 5.4× faster** because pulci batches the changes through one debounce window and exposes the result as a single state read, while manual invocation pays the cold start per file.

Reproduce: `uv run python benchmarks/bench_modes.py --iterations 50` (see [`benchmarks/README.md`](benchmarks/README.md) for methodology and how to expand the fixture).

The MCP tool docstring documents the exact response shapes for `not_running`, `running_no_checks_yet`, `timeout`, `error`, and `tool_errors` so an agent can branch cleanly on each — see [`docs/AGENTS.md`](docs/AGENTS.md#handling-edge-cases) for the full table.

## Diagnose your setup: `pulci doctor`

When something looks wrong, `pulci doctor` tells you which layer is broken
without running the daemon:

```bash
$ pulci doctor
Configuration
  ✓ project root       /home/me/my-project
  ✓ pulci.toml         valid; enabled hooks: ruff, ty, pytest

Tool resolution
  ✓ ruff               0.7.4     via local-venv  .venv/bin/ruff
  ✓ ty                 0.0.3     via local-venv  .venv/bin/ty
  ✓ pytest             8.3.0     via local-venv  .venv/bin/pytest

Filesystem
  ✓ .pulci/ writable   .pulci

Daemon
  ✓ running            not running (run `pulci start` to launch)

State
  ✓ state.json         not present (daemon never ran)

All checks passed.
```

It catches typos in `pulci.toml` (like `clipy = true` inside `[hooks]`),
missing tools, permission issues, and corrupted state files. Exits 0 when
clean, 1 if anything's broken. Add `--json` for structured output.

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
ruff         = true    # ruff check on changed .py files (default: true)
ruff_format  = false   # ruff format --check; format violations as diagnostics (default: false)
ty           = true    # ty check on changed .py files (default: true)
pytest       = false   # pytest on matching test files (default: false)
timeout_secs = 120     # per-hook subprocess timeout in seconds (default: 120)

# How pytest maps source → tests. Each template substitutes {stem} for the
# source's file stem. Empty (default) keeps `tests/test_{stem}.py` only.
pytest_test_patterns = [
    "tests/test_{stem}.py",
    "test/test_{stem}.py",
]

[tools]            # optional — pin exact versions for reproducibility
ruff   = "0.7.4"   # uses uvx ruff@0.7.4 instead of auto-resolving
ty     = "0.0.3"

[watch]            # optional — paths to skip from initial scan and events
exclude = ["vendor", "fixtures"]
```

If `pulci.toml` is absent, defaults apply (`ruff=true`, `ty=true`, `pytest=false`, `ruff_format=false`, timeout 120 s).
Unknown keys fail at startup (typos like `clipy` are rejected loudly rather than silently ignored).
If `[tools]` is absent, pulci resolves each tool automatically: `.venv/bin/<tool>` → `$PATH` → `uvx <tool>`. Pinned versions are probed at daemon startup, so a typo or unpublished version fails fast.

pulci watches `.py` files only (`.rs` is included for internal Rust dogfooding, not a user-facing feature). Non-Python files are ignored without notice.

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

**Scope of `diagnostics`.** Each `state.json` write is the *live aggregated
state* of every source file the daemon has observed. Editing one file
updates that file's entry; the rest stay as they were last checked. Files
deleted from disk are removed from the aggregate (via watcher events and
filesystem reconciliation). Asking "is the project clean?" gets back the
live truth, not just the last batch.

**`stale: true`** appears when pulci detects that the tool binaries
changed between daemon runs (e.g. ruff was upgraded in the venv). It
goes back to `false` after the next check pass. Treat a `stale: true`
state as a fresh full re-check, not stale data.

**`summary.checks_run`** is a monotonic counter incremented on every check
pass since the daemon's first run, persisted across restarts via the prior
`state.json`. It tells consumers "how much work has the daemon done",
useful for `since_version`-style polling.

- Full schema: [`schemas/state.v1.schema.json`](schemas/state.v1.schema.json)
- Config schema: [`schemas/pulci-toml.schema.json`](schemas/pulci-toml.schema.json)
- Agent documentation: [`docs/AGENTS.md`](docs/AGENTS.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md).

## License

[Apache-2.0](LICENSE)
