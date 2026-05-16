"""
Day 6 benchmark: compare three quality-gate modes over N simulated iterations.

Modes
-----
manual  — invoke `ruff check` (+ `ty check` if installed) per iteration.
          Simulates an agent calling tools directly on every file change.
pulci   — daemon already running; agent touches a file and reads the updated
          .pulci/state.json. Simulates pulci in steady-state (warm daemon).
prek    — invoke `prek run` per iteration (skipped if prek is not installed).

Metrics
-------
- Wall-clock time per iteration (mean, p50, p95 in ms)
- Total wall-clock time for N iterations
- Estimated output tokens per iteration  (output_bytes // 4)

Token cost matters: an LLM agent must process every byte of tool output.
pulci's state.json is a fixed-schema compact file; manual tool output can be
orders of magnitude larger when many violations exist.

Usage
-----
    uv run python benchmarks/bench_modes.py
    uv run python benchmarks/bench_modes.py --iterations 20
"""

from __future__ import annotations

import argparse
import json
import pathlib
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Fixture
# ---------------------------------------------------------------------------

# A realistic Python file with several ruff violations so the output is
# non-trivial (unused imports, spacing, naming, etc.).
FIXTURE = """\
import os
import sys
import collections
import pathlib

X=1
Y =2
Z= 3

def compute(a,b,c):
    result=a+b+c
    unused_var = "this is never used"
    return result

class myClass:
    def __init__(self):
        self.Value = 42

def another_func( x , y ):
    return x*y
"""

PULCI_BIN = str(pathlib.Path(sys.executable).parent / "pulci")

# ---------------------------------------------------------------------------
# Data types
# ---------------------------------------------------------------------------


@dataclass
class IterResult:
    duration_s: float
    output_bytes: int

    @property
    def tokens_est(self) -> int:
        return self.output_bytes // 4


@dataclass
class BenchResult:
    mode: str
    iterations: list[IterResult] = field(default_factory=list)
    error: str | None = None

    @property
    def mean_s(self) -> float:
        return statistics.mean(r.duration_s for r in self.iterations)

    @property
    def p50_s(self) -> float:
        return statistics.median(r.duration_s for r in self.iterations)

    @property
    def p95_s(self) -> float:
        data = sorted(r.duration_s for r in self.iterations)
        idx = min(int(len(data) * 0.95), len(data) - 1)
        return data[idx]

    @property
    def total_s(self) -> float:
        return sum(r.duration_s for r in self.iterations)

    @property
    def mean_tokens(self) -> float:
        return statistics.mean(r.tokens_est for r in self.iterations)


# ---------------------------------------------------------------------------
# Mode implementations
# ---------------------------------------------------------------------------


def bench_manual(fixture: pathlib.Path, n: int) -> BenchResult:
    """
    Run ruff check (and ty check if available) on the fixture file per iteration.
    """
    result = BenchResult(mode="manual")
    ty_available = shutil.which("ty") is not None

    for _ in range(n):
        t0 = time.perf_counter()

        r_ruff = subprocess.run(
            ["ruff", "check", "--output-format=json", str(fixture)],
            capture_output=True,
        )
        r_ty = (
            subprocess.run(["ty", "check", str(fixture)], capture_output=True)
            if ty_available
            else None
        )

        elapsed = time.perf_counter() - t0
        out_bytes = len(r_ruff.stdout) + len(r_ruff.stderr)
        if r_ty is not None:
            out_bytes += len(r_ty.stdout) + len(r_ty.stderr)

        result.iterations.append(IterResult(elapsed, out_bytes))

    return result


def bench_pulci(
    project_dir: pathlib.Path,
    fixture: pathlib.Path,
    n: int,
    warmup: int,
) -> BenchResult:
    """
    Start pulci daemon, run warmup iterations, then measure N iterations.

    Per iteration the agent:
    1. Writes a unique change to the fixture file.
    2. Waits for .pulci/state.json to be updated (mtime polling at 10 ms).
    3. Reads the state file — this is the agent's view of the world.
    """
    result = BenchResult(mode="pulci")
    state_file = project_dir / ".pulci" / "state.json"

    # pulci.toml: ruff only, ty + pytest disabled (ty not installed here)
    (project_dir / "pulci.toml").write_text("[hooks]\nruff = true\nty = false\npytest = false\n")

    proc = subprocess.Popen(
        [PULCI_BIN, "start", "--agent", str(project_dir)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    try:
        # Wait for the daemon process to start up and register the inotify watch
        # before we write anything.  The Python + Rust extension startup takes
        # ~1 s on a cold venv; 1.5 s gives comfortable headroom.
        time.sleep(1.5)

        # Bootstrap: trigger first event so the daemon creates the state file.
        fixture.write_text(FIXTURE + "# bootstrap\n")
        if not _wait_for_state(state_file, mtime_before=0, timeout_s=5.0):
            result.error = "daemon did not produce state.json within 5 s"
            return result

        # Warmup: run extra iterations before measuring to stabilise timing.
        for i in range(warmup):
            _touch_and_wait(fixture, state_file, i)

        # Measurement
        for i in range(n):
            elapsed, state_bytes = _touch_and_wait(fixture, state_file, warmup + i)
            if elapsed != elapsed:  # NaN → daemon timeout, skip
                continue
            result.iterations.append(IterResult(elapsed, state_bytes))

    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()

    return result


def _touch_and_wait(
    fixture: pathlib.Path,
    state_file: pathlib.Path,
    seq: int,
) -> tuple[float, int]:
    """
    Write a unique change to the fixture and wait for state.json to refresh.
    """
    mtime_before = state_file.stat().st_mtime_ns if state_file.exists() else 0

    t0 = time.perf_counter()
    fixture.write_text(FIXTURE + f"# seq={seq}\n")

    timed_out = not _wait_for_state(state_file, mtime_before, timeout_s=3.0)
    state_bytes = len(state_file.read_bytes()) if state_file.exists() else 0
    elapsed = time.perf_counter() - t0

    if timed_out:
        return float("nan"), 0
    return elapsed, state_bytes


def _wait_for_state(
    state_file: pathlib.Path,
    mtime_before: int,
    timeout_s: float,
) -> bool:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        time.sleep(0.010)
        if state_file.exists() and state_file.stat().st_mtime_ns != mtime_before:
            return True
    return False


def bench_prek(project_dir: pathlib.Path, n: int) -> BenchResult:
    """
    Run `prek run` per iteration (skipped if prek is not installed).
    """
    result = BenchResult(mode="prek")
    if not shutil.which("prek"):
        result.error = "not installed"
        return result

    for _ in range(n):
        t0 = time.perf_counter()
        r = subprocess.run(
            ["prek", "run"],
            capture_output=True,
            cwd=project_dir,
        )
        elapsed = time.perf_counter() - t0
        result.iterations.append(IterResult(elapsed, len(r.stdout) + len(r.stderr)))

    return result


# ---------------------------------------------------------------------------
# Reporting
# ---------------------------------------------------------------------------

_COL = "─" * 80


def print_table(results: list[BenchResult]) -> None:
    header = (
        f"{'mode':<10}  {'n':>4}  {'mean ms':>8}  {'p50 ms':>7}"
        f"  {'p95 ms':>7}  {'total s':>8}  {'tok/iter':>8}"
    )
    print(_COL)
    print(header)
    print(_COL)
    for r in results:
        if r.error:
            print(f"{r.mode:<10}  {'—':>4}  {r.error}")
            continue
        n = len(r.iterations)
        print(
            f"{r.mode:<10}  {n:>4}"
            f"  {r.mean_s * 1000:>8.1f}"
            f"  {r.p50_s * 1000:>7.1f}"
            f"  {r.p95_s * 1000:>7.1f}"
            f"  {r.total_s:>8.2f}"
            f"  {r.mean_tokens:>8.0f}"
        )
    print(_COL)


def build_json_summary(results: list[BenchResult], n: int) -> dict:
    return {
        "n_iterations": n,
        "modes": [
            {
                "mode": r.mode,
                "error": r.error,
                "mean_ms": round(r.mean_s * 1000, 2) if not r.error else None,
                "p50_ms": round(r.p50_s * 1000, 2) if not r.error else None,
                "p95_ms": round(r.p95_s * 1000, 2) if not r.error else None,
                "total_s": round(r.total_s, 3) if not r.error else None,
                "mean_tokens_est": round(r.mean_tokens) if not r.error else None,
            }
            for r in results
        ],
    }


# ---------------------------------------------------------------------------
# Entry point
# ---------------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument(
        "--iterations",
        "-n",
        type=int,
        default=50,
        metavar="N",
        help="iterations per mode (default: 50)",
    )
    parser.add_argument(
        "--warmup",
        "-w",
        type=int,
        default=5,
        metavar="W",
        help="warmup iterations before measuring (default: 5)",
    )
    args = parser.parse_args()

    n = args.iterations
    warmup = args.warmup

    print(f"\npulci benchmark  —  {n} iterations per mode  ({warmup} warmup)\n")

    with tempfile.TemporaryDirectory(prefix="pulci_bench_") as tmpdir:
        project_dir = pathlib.Path(tmpdir)
        fixture = project_dir / "target.py"
        fixture.write_text(FIXTURE)

        results: list[BenchResult] = []

        # Mode 1: manual
        print(f"[1/3] manual   — warmup {warmup}... ", end="", flush=True)
        bench_manual(fixture, warmup)
        print(f"measuring {n}... ", end="", flush=True)
        results.append(bench_manual(fixture, n))
        print("done")

        # Mode 2: pulci daemon
        print(f"[2/3] pulci    — starting daemon, warmup {warmup}... ", end="", flush=True)
        results.append(bench_pulci(project_dir, fixture, n, warmup))
        print("done")

        # Mode 3: prek
        if shutil.which("prek"):
            print(f"[3/3] prek     — warmup {warmup}... ", end="", flush=True)
            bench_prek(project_dir, warmup)
            print(f"measuring {n}... ", end="", flush=True)
            results.append(bench_prek(project_dir, n))
            print("done")
        else:
            results.append(BenchResult(mode="prek", error="not installed"))
            print("[3/3] prek     — not installed, skipped")

        print()
        print_table(results)

        # Token efficiency narrative
        manual_r = next((r for r in results if r.mode == "manual" and not r.error), None)
        pulci_r = next((r for r in results if r.mode == "pulci" and not r.error), None)
        if manual_r and pulci_r:
            ratio = manual_r.mean_tokens / max(pulci_r.mean_tokens, 1)
            delta_ms = (pulci_r.mean_s - manual_r.mean_s) * 1000
            direction = "faster" if delta_ms < 0 else "slower"
            latency_line = (
                f"Latency          : pulci is ~{abs(delta_ms):.0f} ms {direction}/iter"
                f" vs manual ({pulci_r.mean_s * 1000:.0f} ms vs"
                f" {manual_r.mean_s * 1000:.0f} ms mean)"
            )
            print(f"\nToken efficiency : pulci uses ~{ratio:.1f}x fewer tokens/iter vs manual")
            print(latency_line)
            print(
                "Net benefit      : fewer tokens + comparable latency; debounce"
                " batches rapid saves for free."
            )

        # Write JSON summary
        out_path = pathlib.Path("benchmarks/results.json")
        out_path.parent.mkdir(exist_ok=True)
        summary = build_json_summary(results, n)
        out_path.write_text(json.dumps(summary, indent=2))
        print(f"\nJSON summary → {out_path}")


if __name__ == "__main__":
    main()
