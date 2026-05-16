# pulci

> Continuous quality gate daemon for agent-driven Python development.

**v0.0.1** — Apache-2.0

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
| **pulci**     | **Iteration time**  | **Structured JSON**   | **Agents**  |

pulci does **not** replace any of these. It fills the empty quadrant.

## Install

```bash
git clone https://github.com/gregocasparri/pulci
cd pulci
uv sync
uv run maturin develop --release
uv run pulci --version   # 0.0.1
```

## Usage

**Start the daemon** (runs in foreground; press Ctrl-C to stop):

```bash
pulci start                   # watches current directory, human output
pulci start /path/to/project  # explicit root
pulci start --agent           # compact JSON events — use this in agent loops
```

**Agent mode output** (one JSON line per check):

```json
{"event":"check","files":2,"errors":3,"warnings":1,"checks_run":2,"stale":false}
```

**Query current state** (reads `.pulci/state.json`):

```bash
pulci status          # human-readable table
pulci status --json   # full JSON for agents
```

Sample `pulci status --json` output:

```json
{
  "schema_version": 1,
  "timestamp": "2026-05-16T12:00:00Z",
  "summary": { "errors": 2, "warnings": 1, "checks_run": 2, "stale": false },
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
```

If `pulci.toml` is absent, defaults apply (`ruff=true`, `ty=true`, `pytest=false`).

## Benchmark

Compares three quality-gate modes over 50 iterations on a realistic Python file:

```
uv run python benchmarks/bench_modes.py
```

Typical results (warm daemon, Linux, Ryzen 7):

```
────────────────────────────────────────────────────────────────────────────────
mode        n    mean ms   p50 ms   p95 ms    total s   tok/iter
────────────────────────────────────────────────────────────────────────────────
manual     50      88.4     87.1     96.3       4.42       1111
pulci      50      65.2     63.8     74.1       3.26        383
prek       —   not installed
────────────────────────────────────────────────────────────────────────────────

Token efficiency : pulci uses ~2.9x fewer tokens/iter vs manual
Latency          : pulci is ~23 ms faster/iter vs manual
Net benefit      : fewer tokens + comparable latency; debounce batches rapid saves for free.
```

pulci's compact `state.json` is a fixed-schema file; manual tool output grows
linearly with the number of violations.

## State file contract

`.pulci/state.json` is the primary contract between pulci and consumers.
Schema version is `1` and will be bumped on breaking changes.

Full schema documented in [`docs/AGENTS.md`](docs/AGENTS.md).

## Roadmap

- [x] **Day 1** — scaffold, `pulci --version` end-to-end through Rust + PyO3
- [x] **Day 2** — file watcher (`notify` crate), `pulci start [path]`, ignore filters
- [x] **Day 3** — `Hook` trait, ruff + ty adapters, parallel execution via tokio
- [x] **Day 4** — aggregated JSON state, atomic write, `pulci status --json`, hash cache
- [x] **Day 5** — `pulci.toml` config, `--agent` output mode, selective pytest adapter
- [x] **Day 6** — benchmark suite: manual vs pulci vs prek over 50 iterations
- [x] **Day 7** — README, demo script, metadata, `[dependency-groups]` migration

After v0.1: MCP server interface, mypy/basedpyright/bandit adapters, hosted
team-wide state, marketplace of community hooks.

## License

[Apache-2.0](LICENSE)
