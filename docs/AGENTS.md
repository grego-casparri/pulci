# Using pulci from an AI agent

This document is for AI coding agents (Claude Code, Cursor, Codex,
custom harnesses). If you're a human, the [README](../README.md)
is what you want.

> This document describes the pulci contract as of **v0.0.6** (schema_version 1).

## MCP setup (recommended)

If your host supports MCP (Claude Desktop, Cursor), pulci can install itself:

```bash
pulci mcp install claude-desktop      # writes to ~/Library/Application Support/Claude/...
pulci mcp install cursor              # writes to .cursor/mcp.json (project-local)
pulci mcp install cursor --global     # writes to ~/.cursor/mcp.json (all projects)
```

The installer merges into existing `mcpServers` entries (preserves other servers
you have configured) and writes atomically (tmp + rename). Add `--dry-run` to
preview without touching disk.

For Claude Code: use its native `claude mcp add pulci $(which pulci) mcp`.

If you'd rather paste the config block manually, `pulci mcp info` prints it.

## The contract

pulci runs as a daemon in the project root. It watches the filesystem
and keeps an up-to-date JSON state of every configured quality gate.

**You do not invoke tools directly. You ask pulci for the current state.**

## The one command you need

```bash
pulci status --json
# If the daemon is watching a specific path, pass it here too:
pulci status /path/to/project --json
```

This returns the aggregated diagnostics. Shape:

```json
{
  "schema_version": 1,
  "state_version": 7,
  "timestamp": "2026-05-15T14:23:01Z",
  "summary": {
    "errors": 2,
    "warnings": 1,
    "checks_run": 4,
    "stale": false
  },
  "tools": [
    {"name": "ruff", "version": "0.7.4", "source": "local-venv", "path": ".venv/bin/ruff"},
    {"name": "ty",   "version": "0.0.3", "source": "uvx-latest", "path": null}
  ],
  "diagnostics": [
    {
      "tool": "ruff",
      "file": "src/api.py",
      "line": 42,
      "col": 5,
      "severity": "error",
      "code": "F401",
      "message": "imported but unused"
    }
  ]
}
```

Severity values: `"error"`, `"warning"`, `"info"`. Most adapters emit only `"error"` and `"warning"`. Handle `"info"` gracefully (do not treat as an error).

### Scope of `diagnostics`: last check pass, not project-wide

**Important contract detail.** Each `state.json` write is a snapshot of the
files just checked, not a cumulative view of every problem in the project.

- At daemon startup, the initial sweep checks every source file and writes
  a state with every diagnostic found across the project.
- After that, each file change triggers a check pass on the changed files
  only. The resulting `state.json` carries the diagnostics from *that* pass
  — replacing the previous state, not merging into it.

Concretely: if the initial sweep found 2397 errors across 156 files, and
you then edit `foo.py` (which has 4 errors), the next `state.json` shows
`summary.errors == 4` and 4 diagnostics — all in `foo.py`. The 2393 errors
in the other 155 files **did not disappear**; they're just not in the
current snapshot.

If the question you want answered is "did my edit introduce a regression?",
this is exactly what you want — the snapshot tells you about the change.
If the question is "is the project clean?", you need to consult the post-
sweep state (preserved in git history of `state.json` between runs is not
maintained, so you'd restart the daemon to get a fresh full snapshot).

Per-file accumulating state — where editing `foo.py` cleans only its own
diagnostics and leaves everyone else's intact — is tracked for a future
release. For 0.0.x, "what just changed" is the working semantic.

### `summary.checks_run`

Number of hook adapters that ran in the last check pass. With ruff + ty
enabled this is 2 every pass — it is not a cumulative counter of all
check passes since daemon start.

### `tool_errors` field

A separate array next to `diagnostics`. Entries appear when a hook failed to
produce a verdict — timed out, was killed by a signal, or crashed parsing its
own output:

```json
"tool_errors": [
  {"tool": "pytest", "message": "pytest timed out after 120s and was killed by pulci"}
]
```

When `tool_errors` is non-empty, the listed tools did **not** check the current
state. Absence of diagnostics from those tools is not a clean signal — it is
"no verdict yet". Agents should either retry after a moment, or proceed
explicitly aware that one tool's input is missing. `summary.checks_run` counts
the tool as having executed, so do not use it as a success indicator on its own;
read `tool_errors` first.

## Workflow

After every edit you make, before deciding the next action:

1. Call `pulci status --json`
2. Parse the `summary` field
3. Check `tool_errors` — if non-empty, one or more tools produced no verdict
4. If `summary.errors == 0 && summary.warnings == 0 && tool_errors is empty`, the change is clean
5. Otherwise, the `diagnostics` array tells you what's wrong (`tool_errors` tells you which tools never ran)
6. Fix and loop

Do **not** run `ruff check`, `ty check`, or `pytest` directly while
pulci is active. You'll pay cold start each time and the daemon's
cache will be redundant. The whole point is that pulci already ran
them in the background while you were thinking.

## Schemas and stable contracts

Machine-readable contracts for both the state file and the config:

- [`schemas/state.v1.schema.json`](../schemas/state.v1.schema.json) — full schema for `.pulci/state.json`
- [`schemas/pulci-toml.schema.json`](../schemas/pulci-toml.schema.json) — full schema for `pulci.toml`

Stable behavioural contracts — atomic state writes, monotonic `state_version`,
single-instance daemon, exit codes, schema versioning policy — are documented
in [`docs/ARCHITECTURE.md`](ARCHITECTURE.md#invariants-and-guarantees). Read
that section if you are building an integration that depends on any of them.

## pulci_status tool (MCP)

When using pulci via MCP, call `pulci_status` after each edit instead of
`pulci status --json`. The tool returns the same schema as `state.json`.

If the daemon is not running, the tool returns:
```json
{"status": "not_running", "hint": "run pulci start in your project root"}
```

This is **not** an error — handle it by informing the user to start the daemon.
The tool never sets `isError: true` (the MCP protocol field that marks a tool
call as failed) for a missing daemon. A missing daemon is a valid state, not a
tool failure.

### Causal synchronisation: since_version

`pulci_status` accepts two optional parameters that let you avoid polling
or fixed sleeps when waiting for a fresh result after an edit:

| Parameter       | Type           | Default | Description                                                  |
|-----------------|----------------|---------|--------------------------------------------------------------|
| `since_version` | `int \| None`  | `None`  | Block until `state_version > since_version`.                 |
| `timeout_ms`    | `int`          | `5000`  | Max wait in ms. Returns `{"status": "timeout"}` if exceeded. |

**Recommended agent loop:**

```python
# 1. Get current version before your edit
v = (await pulci_status()).get("state_version", 0)

# 2. Make your edit
# edit foo.py ...

# 3. Wait for the daemon to process it — zero sleep, zero polling
result = await pulci_status(since_version=v)
# result is fresh state; read diagnostics and decide
```

`since_version` is the key to causal correctness: it guarantees you wait for
a result produced *after* your edit, even if the daemon is very fast. There
is no per-file wait — the daemon produces a single global state and every
edit advances `state_version`, so waiting for a version strictly greater
than yours is the right primitive.

### Handling edge cases

`pulci_status` (and `pulci status --json`) returns one of these shapes.
Match on the keys before reading `summary` or `diagnostics`.

| Response shape (key fields)                                       | What it means                                              | Recommended action                                                            |
|-------------------------------------------------------------------|------------------------------------------------------------|-------------------------------------------------------------------------------|
| `{"status": "not_running", "hint": ...}`                          | No daemon (no heartbeat or heartbeat older than 120 s).    | Surface to the user; offer to run `pulci start <path>`.                       |
| `{"status": "running_no_checks_yet", "daemon_status": "alive"}`   | Daemon started; initial scan still in progress.            | Brief retry (~200 ms) before reading state. Almost always resolves in <2 s.   |
| `{"status": "timeout", "hint": ...}`                              | `since_version` was not surpassed within `timeout_ms`.     | Re-call without `since_version` to see current state. If state looks healthy (heartbeat alive, no `tool_errors`) the daemon may have skipped the change as a no-op. |
| `{"status": "error", "hint": ...}`                                | `state.json` is corrupted on disk.                         | Stop and restart the daemon. Surface to the user; don't retry blindly.        |
| Full state with `tool_errors` non-empty                           | One or more hooks produced no verdict (timeout, crash).    | Decide: retry once (transient) or proceed acknowledging the missing verdict. Do NOT treat absence of diagnostics from those tools as success. |
| Full state, `summary.errors == 0 && summary.warnings == 0 && tool_errors == []` | Clean.                                       | Proceed.                                                                       |

For long-lived agents that keep `since_version` across daemon restarts:
the counter is persisted (the new daemon seeds from the prior
`state.json`), so a cached `since_version` from an older daemon is still
valid against a fresh process.

## Adapter version compatibility

pulci exposes diagnostic codes from the underlying tools (e.g. `ruff/F401`).
These codes are stable within the following version ranges:

| Tool   | Supported range | Notes                          |
|--------|-----------------|--------------------------------|
| ruff   | 0.4.x – 0.11.x  | Code names stable across range |
| ty     | 0.0.1 – 0.0.x   | Pre-stable; treat as best-effort |
| pytest | 7.x – 8.x       | Exit codes and output stable   |

If the resolved tool version falls outside these ranges, pulci continues to
run but diagnostic codes may not match the documented values. Read the `tools`
field in `state.json` to verify the active version before acting on codes.

## Agent-mode startup

If you start the daemon yourself (rather than having the user start it), use
`--agent` mode:

```bash
pulci start /path/to/project --agent
```

`--agent` suppresses the human-readable startup messages ("resolved: ...", "Watching ...").
Diagnostic output is always compiler-style regardless of mode — read state via
`pulci status --json`, not by parsing stdout.

### Single-instance guarantee

Only one pulci daemon may watch a given project path at a time. The daemon
acquires an advisory exclusive lock on `.pulci/daemon.lock` at startup. If
you launch a second `pulci start` against a project that already has a live
daemon, the second invocation exits non-zero with a message like:

```
another pulci daemon is already running for this project (lock: .pulci/daemon.lock)
```

The lock is released automatically when the daemon process exits (clean
shutdown, signal, or crash). No manual cleanup is required.

Structured exit events are emitted to stdout on lifecycle changes:

On Ctrl-C or graceful stop:
```json
{"event":"stopped"}
```

On unrecoverable error:
```json
{"event":"error","message":"..."}
```

## Cost discipline

`pulci status --json` is cheap: a single file read, no subprocess,
typically <5ms and <2KB of output. Call it freely.

`"stale": true` means the tool binaries changed between daemon runs (e.g. ruff
was updated in the venv). The daemon detected the change and re-resolved tools.
When `stale` is true, treat the current diagnostics as a full re-check, not an
incremental one. It returns to `false` on the next check pass.

**Baseline state available shortly after startup:** `pulci start` runs a full
project scan immediately after the watcher initialises, so `.pulci/state.json`
is populated within seconds of daemon start. `pulci status` works without
needing to touch a file first. The short window between `pulci start` and
the scan completing is reported consistently across surfaces as
`daemon_status: "alive"` with `status: "running_no_checks_yet"` — both the
CLI and the MCP tool return this shape so agents reading either get the same
signal.

## What pulci is not

pulci does not fix code. It reports state. The fixing is your job.
Tools have their own `--fix` flags; invoke those deliberately when
you want to apply autofixes, not as a background process.

pulci does not run on commit. That's prek/pre-commit territory.

## Configuration

The user's `pulci.toml` defines which hooks are active. You can read it,
but you should not modify it without the user's explicit instruction.
