# pulci

> **Continuous quality gate daemon for agent-driven Python development.**
> Rust core, Python CLI, designed to be consumed by AI agents iterating
> on Python code — not by humans committing.

This file is ground truth for every Claude Code session on this project.
Read it first, every time.

---

## The thesis (one sentence)

A stack of tools that raises the reliability of the development process and
enables high-iteration-speed, agent-delegated work — by closing the gap
between commit-time tooling (prek, pre-commit) and the actual loop an agent runs.

## The market gap we're filling

| When checks run     | Output format        | Existing tools                   |
|---------------------|----------------------|----------------------------------|
| Commit time         | Human terminal       | prek, pre-commit, MegaLinter     |
| Iteration time      | Human terminal       | pytest-watch, ruff --watch       |
| Commit time         | Agent-targeted       | Claude Code Skills, CursorRules  |
| **Iteration time**  | **Agent-targeted**   | **← pulci. Empty quadrant.**     |

Validated May 2026: no standalone tool occupies the bottom-right quadrant.
Adjacent solutions exist (verification loops baked into agent harnesses
like NousResearch/hermes-agent, gitstory) but none extracted as a primitive.

---

## Current state

**Day 1: DONE.** Scaffold complete: Rust workspace (`pulci-core` + `pulci-py`),
Python package (`python/pulci/`), maturin build wired up, `pulci --version`
flows end-to-end through PyO3 bindings. Smoke tests pass. CI on Ubuntu + macOS,
Python 3.10–3.13.

Verify Day 1 is intact before doing anything else:
```bash
uv sync
uv run maturin develop --release
uv run pulci --version    # expects 0.0.1
uv run pytest             # expects 2 tests passing
```

If any of those fail, fix them first. Do not start Day 2 on a broken Day 1.

---

## The roadmap

- [x] **Day 1** — scaffold, `pulci --version` end-to-end
- [ ] **Day 2** — file watcher in `pulci-core` using `notify` crate. Wire to `pulci start [path]`. Filter standard ignores (`.git`, `__pycache__`, `node_modules`, `.venv`, `target`). Single-process daemon, no backgrounding yet.
- [ ] **Day 3** — `Hook` trait in Rust. Adapters for `ruff check --output-format=json` and `ty check`. Parallel execution via `tokio::spawn`. Output collected into in-memory diagnostics.
- [ ] **Day 4** — JSON state aggregator. Atomic write to `.pulci/state.json`. CLI `pulci status` (human) and `pulci status --json` (agent). Hash-based cache to skip unchanged files.
- [ ] **Day 5** — `pulci.toml` config schema + loader. `--agent` output mode (compact JSON, no ANSI, stable error codes). Selective pytest adapter (only tests affected by the changed file).
- [ ] **Day 6** — Benchmark suite. Compare three modes over 50 simulated iterations: manual `ruff + ty` invocation per iteration, `pulci` daemon warm, `prek run` per iteration. Measure total time + estimated output tokens.
- [ ] **Day 7** — Polish: README final, demo GIF (asciinema), make repo public but no announce yet.

Public launch happens in **Week 2**, not Week 1.

---

## Architecture decisions (and rationale)

Full reasoning lives in `docs/ARCHITECTURE.md`. The short version:

- **Rust core + Python wrapper.** Same Astral playbook (`uv`, `ruff`, `ty`). Rust handles speed-critical: file watching, parallel subprocess orchestration, JSON aggregation, caching. Python handles ergonomic surface: CLI, config, install.
- **Workspace layout.** `crates/pulci-core` is pure Rust, no Python dep — reusable later in non-Python contexts. `crates/pulci-py` is the PyO3 shim. `python/pulci/` is the CLI + glue.
- **Daemon, not one-shot CLI.** Each iteration would pay tool cold-start otherwise. Daemon keeps tools warm, maintains incremental cache, exposes consultable state.
- **JSON state as the primary contract.** Agents read state, they don't re-invoke tools. The schema is in `docs/AGENTS.md` and is versioned (`schema_version: 1`).
- **Adapters are thin.** We do not reimplement linting. We shell out to the canonical tool and normalize its output. Cheaper to maintain, inherits upstream quality.

---

## Working style on this project

This is what the maintainer (me, the human) values. Respect this in every PR:

- **Speed is a feature.** If something can be milliseconds, it should be. No 30-second cold starts for things that should be 100ms.
- **Astral conventions.** Apache-2.0, single-binary distribution where possible, structured output as a first-class citizen, `ruff` for everything that ruff covers.
- **Event-driven and hexagonal where it matters.** Hook adapters are *ports*. The orchestrator depends on the trait, not on concrete tools. Adding mypy/basedpyright later should be one file.
- **No premature abstraction.** Day 2 ships a file watcher. Not a framework for watchers.
- **Dogfood from Day 1.** This repo uses `pulci.toml` to check itself once Day 5 lands.

---

## Out of scope (deliberately, until v0.2+)

These come up naturally during work. Resist:

- **Autofixing.** Tools have `--fix`. We report state, we don't mutate code.
- **Pre-commit replacement.** `prek` owns commit-time cleanly. Don't compete.
- **CI execution.** MegaLinter and friends own that. Daemon doesn't make sense in ephemeral containers.
- **MCP server interface.** Post-v0.1. CLI + JSON state file is good enough for now.
- **Anything that adds a runtime dep we don't strictly need.** Resist `serde_yaml`, `clap`, `regex` unless there's no alternative.

---

## Quality bar

- All Rust code: `cargo clippy -- -D warnings` clean. No `unwrap()` outside tests.
- All Python code: `ruff check` and `ruff format` clean.
- Every public function has at least one test.
- CI must be green on `main` always. Broken `main` is a same-day fix.
- Benchmarks tracked across versions — `pulci status --json` should stay under 10ms warm.

---

## Stack and conventions

- **Python:** 3.10+ (we use `Annotated`, structural pattern matching, etc.).
- **Rust:** stable channel, edition 2021.
- **Build:** `uv` for Python env, `maturin` as PyPA build backend.
- **CLI framework:** `typer` for now. May move to Rust `clap` in v0.2 if startup time matters.
- **Async:** `tokio` multi-thread runtime in Rust. No async on the Python side.
- **Errors:** `anyhow` for application, `thiserror` for library boundaries.
- **Logging:** `tracing` (not `log`). User-facing output is separate from internal traces.
- **License:** Apache-2.0 on every source file header (eventually).

---

## How to interact with the maintainer

The maintainer prefers:

- **Direct technical reasoning over hedging.** "I'd do X because Y" beats "we could consider X".
- **Show benchmarks for performance claims.** No vibes-based perf assertions.
- **Spanish or English is fine.** Code and identifiers stay in English.
- **Push back on bad ideas.** Sycophancy is worse than friction.
- **No emojis in code or docs.** Only in casual chat replies.

---

## Useful context outside this file

- `docs/ARCHITECTURE.md` — full design rationale
- `docs/AGENTS.md` — how AI agents (including you) should consume pulci
- `README.md` — public-facing pitch
- The empty quadrant in the README comparison table is the entire reason this project exists. Internalize that.
