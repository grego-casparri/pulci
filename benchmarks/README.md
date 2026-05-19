# pulci benchmarks

Reproducible measurements comparing how a Python agent loop pays for
quality gates under three quality-gate strategies:

- **`manual`** — the agent invokes `ruff`, `ty`, and `pytest` as
  subprocesses on every iteration. Models the naïve loop where every
  file save re-runs the full tool set.
- **`pulci`** — the daemon runs the same tool set in the background;
  the agent observes results by reading `.pulci/state.json` (or by
  calling the MCP `pulci_status` tool, which is equivalent).
- **`prek`** — a pre-commit-style runner (`prek run --all-files`) that
  invokes the same tools per iteration. Configured via a generated
  `.pre-commit-config.yaml` using `language: system` so it calls the
  same binaries as the other modes (no parallel toolchain).

Two scenarios are measured:

- **Steady iteration** — N consecutive single-file edits with a 200 ms
  stability window between them. Models an agent making one edit at a
  time and waiting for results.
- **Burst** — K files modified with no inter-write delay, then a single
  read of the combined result. Models multi-file refactors and code
  modifications.

Per-iteration metrics: wall-clock latency (mean / p50 / p95) and token
cost (the number of tokens the agent consumes to read the tool output).
Tokens are counted with `tiktoken cl100k_base` when the library is
installed; otherwise `bytes ÷ 4` is used and labelled as such.

## How to run

```bash
uv sync
uv run maturin develop --release    # build the pulci Rust extension
uv tool install prek                # optional, for the third mode
.venv/bin/python benchmarks/bench_modes.py --iterations 50
```

The script prints the latency + token tables to stdout and writes the
full result set to `benchmarks/results.json`. Each run uses a fresh
`tempfile.TemporaryDirectory`, copies the fixture there, `git init`s
it (prek requires a git context), and tears everything down on exit.

Override iterations with `--iterations N`. The first run with prek
takes longer because of cold caches; rerun to get steady-state numbers.

## Fixture composition

`benchmarks/fixture/` is a realistic mini-project under
`sampleapp/` with deliberate violations spread across modules so the
tools always have work to do:

- **28 Python files** (`benchmarks/fixture/sampleapp/`)
- **11 ruff violations** (mix of `E501` long lines and `B006` mutable
  defaults — non-autofixable so the violation count stays stable
  across iterations)
- **2 ty type errors**
- **1 failing pytest test** (out of 40 total)

The fixture stays small on purpose so each iteration runs in
under a second and N=50 fits in roughly 30 seconds. If you want to
measure scaling, copy the `sampleapp/` package N times under
different names — the tools will pick them all up.

## Methodology notes

**Apples-to-apples tool sets.** All three modes run ruff + ty + pytest
on every iteration. prek uses `language: system` hooks so it invokes
the same binaries as manual and pulci (verified by checking
`shutil.which`). No mode skips a tool that another runs.

**Cold start excluded.** Each mode runs 5 warm-up iterations before
the measured N — this absorbs Python startup, ruff lazy-init, and
filesystem cache warm-up. Including the warm-up would flatter modes
that have less warm-up cost (manual) and penalize daemons (pulci).

**No network for prek.** prek's local hooks invoke system binaries
directly, so the first run doesn't pay a hook-download cost. This
matches the steady-state experience after `prek run` has been
invoked at least once.

**Latency definition.** Wall-clock time from the file change (`touch
target` for steady, `write K files` for burst) to "result is readable
by the agent" — for manual/prek that means the subprocess returned,
for pulci that means `state_version` advanced and the snapshot is
stable for 200 ms.

**Token definition.** All bytes the agent would consume to read the
result, fed through `tiktoken cl100k_base` (the encoder Claude /
GPT-4 use). For manual that's the combined stdout+stderr of all
three tool invocations; for pulci that's the JSON of `state.json`;
for prek that's the combined stdout+stderr of `prek run`.

**No mock or stub.** Real `ruff`, `ty`, `pytest`, `prek`, and `pulci`
binaries from the project's `.venv`. The fixture is a real codebase
with real diagnostics; the tools run their full pipelines.

## Caveats and known limitations

- **Single-edit token reduction is modest in 0.0.7+.** Since
  `state.json` now carries the live aggregated project view (see
  CHANGELOG 0.0.7), pulci returns roughly the same token volume as
  re-invoking the tools. The savings show up in burst editing and in
  *not paying the cold start* per iteration.
- **Hardware-dependent.** Tested on WSL2 / Linux 6.6.114 / 9p mount.
  Native Linux is faster; macOS APFS is comparable; Windows
  ReadDirectoryChangesW behaves similarly but the fixture path
  normalization is OS-specific (the daemon handles this — see
  fix(daemon) in 0.0.7).
- **Burst K=5 is arbitrary** — chosen because it represents a
  realistic multi-file refactor without dwarfing the steady-state
  numbers. Higher K amplifies pulci's advantage further.
- **No comparison vs `ruff --watch`.** Deliberately omitted: ruff
  --watch runs ruff only, so comparing it to pulci/manual/prek (which
  run ruff + ty + pytest) would conflate the question.

## Output schema

`benchmarks/results.json` after a run:

```json
{
  "n_iterations": 50,
  "token_method": "tiktoken cl100k_base",
  "environment": {"os": "...", "python": "...", "ruff": "...", ...},
  "fixture": {"files": 28, "ruff_violations": 11, ...},
  "modes": [
    {"mode": "manual", "latency": {"mean_ms": ..., "p95_ms": ...},
     "tokens": {"mean_per_call": ..., "total": ...}},
    {"mode": "pulci",  ...},
    {"mode": "prek",   ...}
  ],
  "burst": [
    {"mode": "manual", "burst_size": 5, "n_bursts": 50,
     "mean_tokens_per_burst": ..., "mean_s_per_burst": ...},
    {"mode": "pulci",  ...}
  ]
}
```

Designed to be ingested by external dashboards or CI for tracking
regressions across releases.

## Sample run (0.0.7, WSL2 / Linux 6.6.114)

Steady iteration (N=50):

| Mode | mean ms | p50 ms | p95 ms | tokens/call |
|---|---:|---:|---:|---:|
| manual | 469 | 470 | 542 | 2,397 |
| pulci  | 397 | 383 | 555 | 2,312 |
| prek   | 630 | 576 | 1,058 | 2,491 |

Burst (K=5, N=50):

| Mode | tokens/burst | bytes/burst | mean s/burst |
|---|---:|---:|---:|
| manual | 12,040 | 42,095 | 2.81 |
| pulci  | 2,337  | 7,678  | 0.52 |

Pulci: 5.2× fewer tokens per burst, 5.4× faster wall-clock per burst.
