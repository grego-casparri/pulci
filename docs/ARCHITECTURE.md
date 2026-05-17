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

## Output format design: compiler-style streaming, JSON state

Two surfaces, two formats — each optimized for its primary consumer.

**`.pulci/state.json` and `pulci status --json`** are JSON: lossless,
schema-versioned, machine-readable. The agent calls `pulci status --json`
after each edit and gets a structured object with typed severity, stable
error codes, file+line+col, and resolved tool metadata.

**`pulci start` streaming and `pulci status` (default)** emit
compiler-style text: `file:line:col: severity[tool/code] message`. This
is the format LLMs parse natively — they have seen millions of lines of
gcc/rustc/Python tracebacks in training. Zero parsing overhead, 40–50%
fewer tokens than equivalent JSON for the same diagnostics.

The rule: **JSON when you need to query, compiler-style when you need to
stream**. Confusing these two surfaces produces either unreadable diffs
in agent prompts or fragile regex parsing of state files.

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

**4-level tool resolver (`crates/pulci-core/src/resolver.rs`):** Each tool is
resolved at daemon startup using a fixed precedence chain:
1. Pinned version in `pulci.toml [tools]` → `uvx name@version`
2. Binary in `.venv/bin/` of the watched project root
3. Binary in system `PATH`
4. `uvx name` (latest) as a zero-config fallback

The resolver runs `name --version` after locating the binary to record the
exact version in `state.json`. This version is re-checked on every daemon
cycle; if it changes (e.g. the user ran `pip install --upgrade ruff`), the
`stale` flag in the next `state.json` write is set to `true` so consumers
know a tool upgrade occurred mid-session.

## Invariants and guarantees

The contracts below are stable across patch releases within a `schema_version`.
Code that touches the daemon, the state file, or the public CLI surfaces must
preserve them. Most are also enforced by tests; the rest are enforced by
review.

**Atomic state write.** `.pulci/state.json` is written by serializing the full
state to `state.json.tmp` and then `rename(2)` into place. POSIX guarantees the
rename is atomic on the same filesystem, so a reader sees either the previous
complete state or the new complete state — never a partial. There is no
incremental update path; every write is a whole `State`.

**Monotonic `state_version`.** Incremented exactly once per `build_state` call,
which happens exactly once per check pass. The counter never decreases within
the lifetime of a daemon. Consumers can use `state_version > last_seen` as the
sole signal that a new result is available (this is the contract that backs
`pulci_status(since_version=...)` per D-013).

**Single instance per project.** `pulci start` acquires an advisory exclusive
lock on `.pulci/daemon.lock` (`fs2::FileExt::try_lock_exclusive`) before any
other I/O — before the heartbeat thread, before the watcher, before the
initial scan. A second `pulci start` against the same project root fails fast
with a non-zero exit code and a message that names the lock path. The kernel
releases the lock on process exit, including SIGKILL.

**Paralelismo.** The tokio runtime is built with `worker_threads(4)`. Hooks
are CPU-light and subprocess-bound — each hook is one `spawn_blocking` task
shelling out to ruff/ty/pytest/clippy. Adding more workers doesn't help
because the bottleneck is the subprocess, not the runtime. This constant
changes only with a benchmark.

**Debounce window.** Events received within 50 ms of the first event in a
batch are coalesced into a single check pass. The window covers the
typical "save + auto-format" double-write of modern editors without
introducing perceptible latency on a single edit. Hardcoded.

**Hook timeout.** Each hook subprocess is killed and reported as a
`tool_errors` entry if it does not exit within the configured timeout.
Default is 120 s — generous enough that legitimate slow runs (cold pytest,
first-run cargo clippy) never trip it; tight enough that a true hang
surfaces within two minutes instead of freezing the daemon indefinitely.
Override via `[hooks] timeout_secs = N` in `pulci.toml` when a real test
suite exceeds the default.

**Exit codes (public contract).**

| Command         | Exit | Meaning                                           |
|-----------------|------|---------------------------------------------------|
| `pulci start`   | 0    | Stopped cleanly (Ctrl-C, graceful shutdown)       |
| `pulci start`   | ≠0   | Lock contention, irrecoverable error              |
| `pulci status`  | 0    | No daemon running, OR daemon clean (no errors)    |
| `pulci status`  | 1    | `summary.errors > 0`                              |
| `pulci status`  | 2    | `state.json` is corrupted                         |
| `pulci mcp`     | 0    | Server stopped cleanly                            |
| `pulci mcp`     | ≠0   | Irrecoverable                                     |

`tool_errors` alone does NOT flip `pulci status` to 1. The decision to act on
a missing verdict is the agent's, not the daemon's.

**Schema versioning.** `state.v1.schema.json` is at `schema_version=1`.
Breaking changes (field renames, type changes, removals, semantic shifts)
bump to `2`. Purely additive fields are allowed within version `1` and use
`#[serde(default)]` in the Rust types so historical `state.json` files
deserialize cleanly. Consumers ignore unknown fields by convention.

**Watcher ignores.** The watcher skips these directories unconditionally:
`.git`, `.pulci`, `.ruff_cache`, `.pytest_cache`, `__pycache__`,
`node_modules`, `.venv`, `target`, and any path starting with
`pytest-cache-files-`. Users can add more via `[watch] exclude` in
`pulci.toml`. The event loop further restricts events to `.py` files
(and `.rs` when `[hooks] clippy = true`) before invoking hooks.

**Rescan on overflow.** If the underlying file-watcher backend reports lost
events (Linux: inotify queue overflow surfaced as `Flag::Rescan` per notify
crate), the daemon does a full `collect_py_files` scan equivalent to the
startup scan. The `FileCache` then filters down to actually-modified files.
This is defense in depth against silent staleness — see the failure-modes
audit for empirical frequency notes.

## Out of scope (deliberately)

These are real problems, but not pulci's:

- **Fixing diagnostics.** Tools already do `--fix`. pulci reports state;
  the agent decides what to do with it.
- **Pre-commit replacement.** prek handles the commit-time gate (`.prek.yaml`
  ships with the repo). pulci runs between commits; prek runs at the commit
  moment. They are complementary, not competing.
- **CI execution.** MegaLinter and friends own that. pulci's daemon
  doesn't make sense in ephemeral CI containers.
