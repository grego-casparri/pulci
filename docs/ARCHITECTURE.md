# Architecture

This document captures the *why* behind pulci's structure. It's written
for your future self and any contributor who wonders "couldn't this have
been simpler / different?"

## Two-language hybrid: Rust core, Python wrapper

The same Astral playbook used by `uv`, `ruff`, `ty`: the engine lives in
Rust, the user interface lives in Python. Why this split:

- **Rust core** handles the work where speed and correctness matter:
  filesystem watching, parallel subprocess orchestration, JSON aggregation,
  cache invalidation. Daemon cold start is sub-100ms.
- **Python wrapper** is the user-facing surface: the CLI (typer),
  the config loader, the install ergonomics.

Distribution will be `pip install pulci` once wheels are published to PyPI
(planned for v0.1 public release). Currently install from source via
`uv run maturin develop --release`.

## Workspace layout

```
crates/pulci-core   pure Rust, no Python dep — reusable as a library
crates/pulci-py     PyO3 bindings, compiles to pulci._native
python/pulci        Python package: CLI, config, glue
```

Keeping the core in its own crate without Python dependencies means
someone can use pulci's machinery from a pure Rust binary later
(e.g. a Rust CLI variant or a different agent harness integration)
without paying a Python tax.

## Why daemon mode, not a one-shot CLI

A naive design: `pulci check` runs all hooks against the current
working tree and exits. This is what `prek run` does.

The problem for agent iteration: each invocation pays cold start
of every tool. ruff's startup is fast (~10ms), but pytest cold start
on a real project can be 1-3 seconds. Multiply by 50 iterations per
hour and you're burning real time on process bring-up.

A persistent daemon:

- Keeps tools warm (ruff has `--watch`, ty will, pytest can hold collection)
- Maintains an incremental cache that survives between iterations
- Watches the filesystem, runs only on actual change
- Exposes consultable state, so the agent's read is O(file read), not O(re-run)

## Why structured JSON output as a first-class citizen

Existing tools target the terminal. Colored ANSI codes, progress bars,
formatted tables — all hostile to LLM consumption. The agent has to:

1. Strip the ANSI
2. Parse the human format (often regex-y, fragile)
3. Reconstruct semantic meaning
4. Spend tokens describing what it found

Every step adds latency and token cost. pulci's `--json` mode emits
the schema the agent actually wants: one diagnostic per object, typed
severity, stable error codes, file+line+col, no narrative.

This is the principle behind the comparison table in the README:
**pulci is the agent-targeted quadrant of the quality-gate space.**

## Hook adapters: contracts, not subclasses

Each integrated tool (ruff, ty, pytest, ...) gets an *adapter* in
`pulci-core/src/hooks/`. An adapter is a Rust struct implementing
the `Hook` trait, with two responsibilities:

1. **Invoke** the underlying tool against a set of files
2. **Parse** its output into the unified `Diagnostic` schema

Adapters are deliberately thin. We don't reimplement linting; we
shell out to the canonical tool and normalize its output. This means:

- We inherit upstream's quality and rule coverage for free
- Adding a new tool is one adapter file, not a fork
- Tool authors don't need to ship anything pulci-specific

The contract is the JSON schema, not a plugin SDK. Cheaper to maintain
on both sides.

## Key implementation details

**Debounce window (50 ms):** The watcher accumulates inotify/FSEvents across a
50 ms window before triggering a check pass. This prevents redundant runs when
editors write multiple files in quick succession (e.g. save + format).

**FileCache (mtime + size):** Between check passes, the orchestrator only feeds
changed files to hooks. The cache key is `(mtime_ns, size_bytes)` from
`fs::metadata`. This avoids re-running ruff on a file that was opened but not
modified (touch without content change resets mtime, so the cache conservatively
re-checks it).

**Tokio runtime inside `start()`:** The tokio multi-thread runtime is created
inside the PyO3 `start()` function, not at module import time. This means Python
can import `pulci._native` without spawning threads. The runtime drives the
`Orchestrator::run` async method which spawns one `tokio::task::spawn_blocking`
per hook — hooks are synchronous subprocesses, so blocking is correct.

**GIL discipline:** `start()` releases the Python GIL during hook execution via
`py.allow_threads(|| ...)` so that Ctrl-C (SIGINT) is delivered by Python's
signal machinery even when tools are running.

## Out of scope (deliberately)

These are real problems, but not pulci's:

- **Fixing diagnostics.** Tools already do `--fix`. pulci reports state;
  the agent decides what to do with it.
- **Pre-commit replacement.** prek owns that cleanly. pulci runs
  between commits, not at the commit moment.
- **CI execution.** MegaLinter and friends own that. pulci's daemon
  doesn't make sense in ephemeral CI containers.
- **MCP server (yet).** Post-v0.1. The CLI + JSON state file is a
  perfectly good agent interface for now.
