"""
Benchmark: compare three quality-gate modes over N simulated iterations.

Modes
-----
manual  — invoke `ruff check` (+ `ty check` if installed) per iteration.
          Simulates an agent calling tools directly on every file change.
pulci   — daemon already running; agent touches a file and reads the updated
          .pulci/state.json. Simulates pulci in steady-state (warm daemon).
prek    — invoke `prek run` per iteration (skipped if prek is not installed).

Metrics
-------
Two independent dimensions — reported in separate tables:

  Latency   : wall-clock time from file change to result available (ms)
  Token cost: tokens the agent must consume to read the result

Token cost uses tiktoken cl100k_base when available; falls back to bytes÷4.
Both methods are labeled clearly in the output so readers can judge validity.

Usage
-----
    uv run python benchmarks/bench_modes.py
    uv run python benchmarks/bench_modes.py --iterations 20
"""

from __future__ import annotations

import argparse
import json
import pathlib
import platform
import re
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass, field

# ---------------------------------------------------------------------------
# Token counting — real tokenizer preferred, bytes÷4 as labeled fallback
# ---------------------------------------------------------------------------

try:
    import tiktoken as _tiktoken

    _ENC = _tiktoken.get_encoding("cl100k_base")

    def _count_tokens(data: bytes) -> int:
        return len(_ENC.encode(data.decode("utf-8", errors="replace")))

    _TOKEN_METHOD = "tiktoken cl100k_base"
except ImportError:

    def _count_tokens(data: bytes) -> int:  # type: ignore[misc]
        return len(data) // 4

    _TOKEN_METHOD = "bytes÷4  (install tiktoken for real counts)"

# ---------------------------------------------------------------------------
# Fixture
# ---------------------------------------------------------------------------

# Multi-file project under benchmarks/fixture/ — realistic code with
# intentional non-autofixable violations (E501, B006) spread across modules.
FIXTURE_DIR = pathlib.Path(__file__).parent / "fixture"

# File touched per iteration to trigger inotify events; has an E501 violation
# so the daemon always sees at least one violation after each write.
_TOUCH_RELATIVE = pathlib.Path("sampleapp") / "utils.py"

# K files written in rapid succession for burst mode — one per package so
# they arrive as distinct inotify events and bypass the FileCache hash check.
_BURST_FILES: list[pathlib.Path] = [
    pathlib.Path("sampleapp") / "utils.py",
    pathlib.Path("sampleapp") / "api" / "auth.py",
    pathlib.Path("sampleapp") / "core" / "cache.py",
    pathlib.Path("sampleapp") / "db" / "queries.py",
    pathlib.Path("sampleapp") / "services" / "notifications.py",
]

PULCI_BIN = str(pathlib.Path(sys.executable).parent / "pulci")
_TY_AVAILABLE = shutil.which("ty") is not None
_PYTEST_AVAILABLE = shutil.which("pytest") is not None

# ---------------------------------------------------------------------------
# Data types
# ---------------------------------------------------------------------------


@dataclass
class IterResult:
    duration_s: float
    output_bytes: int
    output_tokens: int


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
        return statistics.mean(r.output_tokens for r in self.iterations)

    @property
    def total_tokens(self) -> int:
        return sum(r.output_tokens for r in self.iterations)

    @property
    def mean_bytes(self) -> float:
        return statistics.mean(r.output_bytes for r in self.iterations)


@dataclass
class BurstIterResult:
    total_s: float
    calls_made: int  # K for manual; state_version delta for pulci
    total_tokens: int
    total_bytes: int


@dataclass
class BurstResult:
    mode: str
    burst_size: int
    bursts: list[BurstIterResult] = field(default_factory=list)
    error: str | None = None

    @property
    def n(self) -> int:
        return len(self.bursts)

    @property
    def mean_s(self) -> float:
        return statistics.mean(b.total_s for b in self.bursts)

    @property
    def mean_calls(self) -> float:
        return statistics.mean(b.calls_made for b in self.bursts)

    @property
    def mean_tokens(self) -> float:
        return statistics.mean(b.total_tokens for b in self.bursts)

    @property
    def total_tokens(self) -> int:
        return sum(b.total_tokens for b in self.bursts)

    @property
    def mean_bytes(self) -> float:
        return statistics.mean(b.total_bytes for b in self.bursts)


# ---------------------------------------------------------------------------
# Environment and fixture info
# ---------------------------------------------------------------------------


def _env_info() -> dict[str, str]:
    ruff_v = subprocess.run(["ruff", "--version"], capture_output=True, text=True).stdout.strip()
    info: dict[str, str] = {
        "os": f"{platform.system()} {platform.release()}",
        "python": platform.python_version(),
        "ruff": ruff_v,
        "tokens": _TOKEN_METHOD,
    }
    if _TY_AVAILABLE:
        ty_v = subprocess.run(["ty", "--version"], capture_output=True, text=True).stdout.strip()
        info["ty"] = ty_v
    if _PYTEST_AVAILABLE:
        pt_v = subprocess.run(
            ["pytest", "--version"], capture_output=True, text=True
        ).stdout.strip()
        info["pytest"] = pt_v
    return info


def _fixture_stats(project_dir: pathlib.Path) -> dict[str, int]:
    py_files = list(project_dir.rglob("*.py"))
    r_ruff = subprocess.run(
        ["ruff", "check", "--output-format=json", "."],
        capture_output=True,
        cwd=project_dir,
    )
    try:
        ruff_violations = len(json.loads(r_ruff.stdout))
    except (json.JSONDecodeError, ValueError):
        ruff_violations = -1
    stats: dict[str, int] = {"files": len(py_files), "ruff_violations": ruff_violations}
    if _TY_AVAILABLE:
        r_ty = subprocess.run(["ty", "check", "."], capture_output=True, cwd=project_dir)
        stats["ty_errors"] = r_ty.stdout.count(b"error[")
    if _PYTEST_AVAILABLE:
        r_pt = subprocess.run(
            ["pytest", "--tb=no", "-v"], capture_output=True, text=True, cwd=project_dir
        )
        last = r_pt.stdout.strip().splitlines()[-1] if r_pt.stdout.strip() else ""
        m_fail = re.search(r"(\d+) failed", last)
        m_pass = re.search(r"(\d+) passed", last)
        stats["pytest_failed"] = int(m_fail.group(1)) if m_fail else 0
        stats["pytest_passed"] = int(m_pass.group(1)) if m_pass else 0
    return stats


def _print_header(env: dict[str, str], stats: dict[str, int], n: int, warmup: int) -> None:
    W = 56
    sep = "─" * W
    print(f"\n{sep}")
    print("pulci benchmark")
    print(sep)
    print(f"  OS      : {env['os']}")
    print(f"  Python  : {env['python']}")
    print(f"  ruff    : {env['ruff']}")
    if "ty" in env:
        print(f"  ty      : {env['ty']}")
    if "pytest" in env:
        print(f"  pytest  : {env['pytest']}")
    print(f"  tokens  : {env['tokens']}")
    print(sep)
    ruff_v = stats["ruff_violations"]
    fixture_line = f"  Fixture : {stats['files']} Python files, {ruff_v} ruff violations"
    if "ty_errors" in stats:
        fixture_line += f", {stats['ty_errors']} ty errors"
    if "pytest_failed" in stats:
        fixture_line += f", {stats['pytest_failed']} test failures"
    print(fixture_line)
    parts = ["ruff"]
    if _TY_AVAILABLE:
        parts.append("ty")
    if _PYTEST_AVAILABLE:
        parts.append("pytest")
    print(f"  Tools   : {' + '.join(parts)}")
    print(f"  Iters   : {n} measured + {warmup} warmup per mode")
    print(f"{sep}\n")


# ---------------------------------------------------------------------------
# Mode implementations
# ---------------------------------------------------------------------------


def bench_manual(project_dir: pathlib.Path, n: int) -> BenchResult:
    """
    Run ruff check (and ty check if available) on the whole project per iteration.
    """
    result = BenchResult(mode="manual")

    for _ in range(n):
        t0 = time.perf_counter()

        r_ruff = subprocess.run(
            ["ruff", "check", "--output-format=json", "."],
            capture_output=True,
            cwd=project_dir,
        )
        r_ty = (
            subprocess.run(["ty", "check", "."], capture_output=True, cwd=project_dir)
            if _TY_AVAILABLE
            else None
        )
        r_pt = (
            subprocess.run(["pytest", "--tb=short", "-q"], capture_output=True, cwd=project_dir)
            if _PYTEST_AVAILABLE
            else None
        )

        elapsed = time.perf_counter() - t0
        raw = r_ruff.stdout + r_ruff.stderr
        if r_ty is not None:
            raw += r_ty.stdout + r_ty.stderr
        if r_pt is not None:
            raw += r_pt.stdout + r_pt.stderr

        result.iterations.append(
            IterResult(elapsed, output_bytes=len(raw), output_tokens=_count_tokens(raw))
        )

    return result


def bench_pulci(
    project_dir: pathlib.Path,
    touch_target: pathlib.Path,
    n: int,
    warmup: int,
) -> BenchResult:
    """
    Start pulci daemon, run warmup iterations, then measure N iterations.

    Per iteration the agent:
    1. Writes a unique change to touch_target (a file inside project_dir).
    2. Waits for .pulci/state.json to be updated (mtime polling at 10 ms).
    3. Reads the state file — this is the agent's view of the world.
    """
    result = BenchResult(mode="pulci")
    state_file = project_dir / ".pulci" / "state.json"

    _ty = "true" if _TY_AVAILABLE else "false"
    _pt = "true" if _PYTEST_AVAILABLE else "false"
    (project_dir / "pulci.toml").write_text(f"[hooks]\nruff = true\nty = {_ty}\npytest = {_pt}\n")

    original_content = touch_target.read_text()

    proc = subprocess.Popen(
        [PULCI_BIN, "start", "--agent", str(project_dir)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    try:
        # Python + Rust extension startup takes ~1 s on a cold venv; 1.5 s gives headroom.
        time.sleep(1.5)

        touch_target.write_text(original_content + "# bootstrap\n")
        if not _wait_for_state(state_file, mtime_before=0, timeout_s=5.0):
            result.error = "daemon did not produce state.json within 5 s"
            return result

        for i in range(warmup):
            _touch_and_wait(touch_target, original_content, state_file, i)

        for i in range(n):
            elapsed, raw = _touch_and_wait(touch_target, original_content, state_file, warmup + i)
            if elapsed != elapsed:  # NaN → daemon timeout
                continue
            result.iterations.append(
                IterResult(elapsed, output_bytes=len(raw), output_tokens=_count_tokens(raw))
            )

    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()

    return result


def _touch_and_wait(
    touch_target: pathlib.Path,
    original_content: str,
    state_file: pathlib.Path,
    seq: int,
) -> tuple[float, bytes]:
    mtime_before = state_file.stat().st_mtime_ns if state_file.exists() else 0

    t0 = time.perf_counter()
    touch_target.write_text(original_content + f"# seq={seq}\n")

    timed_out = not _wait_for_state(state_file, mtime_before, timeout_s=3.0)
    raw = state_file.read_bytes() if state_file.exists() else b""
    elapsed = time.perf_counter() - t0

    if timed_out:
        return float("nan"), b""
    return elapsed, raw


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
        r = subprocess.run(["prek", "run"], capture_output=True, cwd=project_dir)
        elapsed = time.perf_counter() - t0
        raw = r.stdout + r.stderr
        result.iterations.append(
            IterResult(elapsed, output_bytes=len(raw), output_tokens=_count_tokens(raw))
        )

    return result


# ---------------------------------------------------------------------------
# Burst mode helpers and implementations
# ---------------------------------------------------------------------------


def _read_state_version(state_file: pathlib.Path) -> int:
    try:
        return int(json.loads(state_file.read_bytes()).get("state_version", 0))
    except Exception:
        return 0


def _wait_for_settled(
    state_file: pathlib.Path,
    mtime_before: int,
    stable_ms: int = 200,
    timeout_s: float = 5.0,
) -> bool:
    """
    Wait until state.json has changed AND remained stable for stable_ms.
    """
    deadline = time.monotonic() + timeout_s
    last_mtime = mtime_before
    stable_since: float | None = None

    while time.monotonic() < deadline:
        time.sleep(0.010)
        if not state_file.exists():
            continue
        mtime = state_file.stat().st_mtime_ns
        if mtime != last_mtime:
            last_mtime = mtime
            stable_since = time.monotonic()
        elif stable_since is not None:
            if time.monotonic() - stable_since >= stable_ms / 1000:
                return True
    return False


def bench_burst_manual(
    project_dir: pathlib.Path,
    burst_files: list[pathlib.Path],
    original_contents: list[str],
    n_bursts: int,
    burst_size: int,
) -> BurstResult:
    """
    Simulate K rapid file changes followed by K ruff calls.
    Represents an agent that checks quality after every individual edit.
    """
    result = BurstResult(mode="manual", burst_size=burst_size)
    files = burst_files[:burst_size]
    contents = original_contents[:burst_size]

    for b in range(n_bursts):
        t0 = time.perf_counter()
        total_tokens = 0
        total_bytes = 0
        for i, (f, content) in enumerate(zip(files, contents, strict=True)):
            f.write_text(content + f"# burst={b} i={i}\n")
            r_ruff = subprocess.run(
                ["ruff", "check", "--output-format=json", "."],
                capture_output=True,
                cwd=project_dir,
            )
            raw = r_ruff.stdout + r_ruff.stderr
            if _TY_AVAILABLE:
                r_ty = subprocess.run(["ty", "check", "."], capture_output=True, cwd=project_dir)
                raw += r_ty.stdout + r_ty.stderr
            if _PYTEST_AVAILABLE:
                r_pt = subprocess.run(
                    ["pytest", "--tb=short", "-q"], capture_output=True, cwd=project_dir
                )
                raw += r_pt.stdout + r_pt.stderr
            total_tokens += _count_tokens(raw)
            total_bytes += len(raw)
        elapsed = time.perf_counter() - t0
        result.bursts.append(BurstIterResult(elapsed, burst_size, total_tokens, total_bytes))

    return result


def bench_burst_pulci(
    project_dir: pathlib.Path,
    burst_files: list[pathlib.Path],
    original_contents: list[str],
    n_bursts: int,
    burst_size: int,
    warmup: int,
) -> BurstResult:
    """
    Write K files as fast as possible, wait for state.json to settle, read once.
    Represents pulci steady-state: all K writes land in the 50 ms debounce window.
    """
    result = BurstResult(mode="pulci", burst_size=burst_size)
    state_file = project_dir / ".pulci" / "state.json"
    files = burst_files[:burst_size]
    contents = original_contents[:burst_size]

    _ty = "true" if _TY_AVAILABLE else "false"
    _pt = "true" if _PYTEST_AVAILABLE else "false"
    (project_dir / "pulci.toml").write_text(f"[hooks]\nruff = true\nty = {_ty}\npytest = {_pt}\n")

    proc = subprocess.Popen(
        [PULCI_BIN, "start", "--agent", str(project_dir)],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )

    try:
        time.sleep(1.5)

        files[0].write_text(contents[0] + "# bootstrap\n")
        if not _wait_for_state(state_file, mtime_before=0, timeout_s=5.0):
            result.error = "daemon did not produce state.json within 5 s"
            return result

        for b in range(warmup):
            _do_burst_pulci(files, contents, state_file, seq=b)

        for b in range(n_bursts):
            br = _do_burst_pulci(files, contents, state_file, seq=warmup + b)
            if br is not None:
                result.bursts.append(br)

    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()

    return result


def _do_burst_pulci(
    files: list[pathlib.Path],
    contents: list[str],
    state_file: pathlib.Path,
    seq: int,
) -> BurstIterResult | None:
    mtime_before = state_file.stat().st_mtime_ns if state_file.exists() else 0
    version_before = _read_state_version(state_file)

    t0 = time.perf_counter()

    for i, (f, content) in enumerate(zip(files, contents, strict=True)):
        f.write_text(content + f"# burst={seq} i={i}\n")

    if not _wait_for_settled(state_file, mtime_before, stable_ms=200, timeout_s=5.0):
        return None

    elapsed = time.perf_counter() - t0
    actual_checks = max(1, _read_state_version(state_file) - version_before)
    raw = state_file.read_bytes() if state_file.exists() else b""
    return BurstIterResult(elapsed, actual_checks, _count_tokens(raw), len(raw))


# ---------------------------------------------------------------------------
# Reporting — two separate tables
# ---------------------------------------------------------------------------

_SEP = "─" * 68


def _fmt(n: float) -> str:
    return f"{n:,.0f}"


def print_latency_table(results: list[BenchResult]) -> None:
    print("Latency  (wall-clock from file change to result available)")
    print(_SEP)
    print(f"  {'mode':<10} {'n':>5}  {'mean ms':>9}  {'p50 ms':>8}  {'p95 ms':>8}  {'total s':>8}")
    print(_SEP)
    for r in results:
        if r.error:
            print(f"  {r.mode:<10} {'—':>5}  {r.error}")
            continue
        n = len(r.iterations)
        print(
            f"  {r.mode:<10} {n:>5}"
            f"  {r.mean_s * 1000:>9.1f}"
            f"  {r.p50_s * 1000:>8.1f}"
            f"  {r.p95_s * 1000:>8.1f}"
            f"  {r.total_s:>8.2f}"
        )
    print(_SEP)


def print_token_table(results: list[BenchResult]) -> None:
    print(f"\nToken cost  ({_TOKEN_METHOD})")
    print(_SEP)
    print(
        f"  {'mode':<10} {'n':>5}  {'tok/call':>9}  {'bytes/call':>10}"
        f"  {'total tok':>10}  {'total bytes':>11}"
    )
    print(_SEP)
    for r in results:
        if r.error:
            print(f"  {r.mode:<10} {'—':>5}  {r.error}")
            continue
        n = len(r.iterations)
        print(
            f"  {r.mode:<10} {n:>5}"
            f"  {_fmt(r.mean_tokens):>9}"
            f"  {_fmt(r.mean_bytes):>10}"
            f"  {_fmt(r.total_tokens):>10}"
            f"  {_fmt(sum(it.output_bytes for it in r.iterations)):>11}"
        )
    print(_SEP)


def print_summary(results: list[BenchResult]) -> None:
    manual_r = next((r for r in results if r.mode == "manual" and not r.error), None)
    pulci_r = next((r for r in results if r.mode == "pulci" and not r.error), None)
    if not (manual_r and pulci_r):
        return

    tok_ratio = manual_r.mean_tokens / max(pulci_r.mean_tokens, 1)
    byte_ratio = manual_r.mean_bytes / max(pulci_r.mean_bytes, 1)
    delta_ms = (pulci_r.mean_s - manual_r.mean_s) * 1000
    direction = "faster" if delta_ms < 0 else "slower"

    tok_line = (
        f"\n  Token reduction : {tok_ratio:.1f}x fewer tokens/call"
        f"  ({_fmt(manual_r.mean_tokens)} → {_fmt(pulci_r.mean_tokens)})"
    )
    byte_line = (
        f"  Byte reduction  : {byte_ratio:.1f}x fewer bytes/call"
        f"   ({_fmt(manual_r.mean_bytes)} → {_fmt(pulci_r.mean_bytes)})"
    )
    print(tok_line)
    print(byte_line)
    print(f"  Latency delta   : pulci is {abs(delta_ms):.0f} ms {direction}/call vs manual")
    print(
        "\n  Note: pulci latency includes inotify debounce (intentional — batches"
        "\n  rapid saves). Token reduction compounds across N iterations."
    )


def print_burst_table(burst_results: list[BurstResult]) -> None:
    if not any(br for br in burst_results if not br.error):
        return
    k = next((br.burst_size for br in burst_results if not br.error), 0)
    print(f"\nBurst scenario  (K={k} files/burst, writes with no delay between them)")
    print(_SEP)
    print(
        f"  {'mode':<10} {'bursts':>7}  {'tok/burst':>10}"
        f"  {'bytes/burst':>12}  {'total tok':>10}  {'mean s':>8}"
    )
    print(_SEP)
    for br in burst_results:
        if br.error:
            print(f"  {br.mode:<10} {'—':>7}  {br.error}")
            continue
        print(
            f"  {br.mode:<10} {br.n:>7}"
            f"  {_fmt(br.mean_tokens):>10}"
            f"  {_fmt(br.mean_bytes):>12}"
            f"  {_fmt(br.total_tokens):>10}"
            f"  {br.mean_s:>8.3f}"
        )
    print(_SEP)


def print_burst_summary(burst_results: list[BurstResult]) -> None:
    manual_r = next((br for br in burst_results if br.mode == "manual" and not br.error), None)
    pulci_r = next((br for br in burst_results if br.mode == "pulci" and not br.error), None)
    if not (manual_r and pulci_r):
        return

    tok_ratio = manual_r.mean_tokens / max(pulci_r.mean_tokens, 1)
    byte_ratio = manual_r.mean_bytes / max(pulci_r.mean_bytes, 1)
    saved = manual_r.total_tokens - pulci_r.total_tokens
    delta_ms = (pulci_r.mean_s - manual_r.mean_s) * 1000
    direction = "faster" if delta_ms < 0 else "slower"

    print(
        f"\n  Token reduction : {tok_ratio:.1f}x fewer tokens/burst"
        f"  ({_fmt(manual_r.mean_tokens)} → {_fmt(pulci_r.mean_tokens)})"
    )
    print(
        f"  Byte reduction  : {byte_ratio:.1f}x fewer bytes/burst"
        f"  ({_fmt(manual_r.mean_bytes)} → {_fmt(pulci_r.mean_bytes)})"
    )
    print(f"  Time per burst  : pulci {abs(delta_ms):.0f} ms {direction} vs manual")
    print(
        f"  Total savings   : {_fmt(saved)} tokens across {pulci_r.n} bursts"
        f" ({_fmt(manual_r.total_tokens)} → {_fmt(pulci_r.total_tokens)})"
    )
    print(
        "\n  Note: burst time includes inotify debounce (50 ms) + ruff run + "
        "200 ms\n  stability window. In real agent use the MCP blocking path "
        "eliminates polling."
    )


def build_json_summary(
    results: list[BenchResult],
    burst_results: list[BurstResult],
    n: int,
    env: dict[str, str],
    stats: dict[str, int],
) -> dict:
    return {
        "n_iterations": n,
        "token_method": _TOKEN_METHOD,
        "environment": env,
        "fixture": stats,
        "modes": [
            {
                "mode": r.mode,
                "error": r.error,
                "latency": {
                    "mean_ms": round(r.mean_s * 1000, 2),
                    "p50_ms": round(r.p50_s * 1000, 2),
                    "p95_ms": round(r.p95_s * 1000, 2),
                    "total_s": round(r.total_s, 3),
                }
                if not r.error
                else None,
                "tokens": {
                    "mean_per_call": round(r.mean_tokens),
                    "total": r.total_tokens,
                    "mean_bytes_per_call": round(r.mean_bytes),
                    "total_bytes": sum(it.output_bytes for it in r.iterations),
                }
                if not r.error
                else None,
            }
            for r in results
        ],
        "burst": [
            {
                "mode": br.mode,
                "burst_size": br.burst_size,
                "error": br.error,
                "n_bursts": br.n if not br.error else None,
                "mean_calls_per_burst": round(br.mean_calls, 2) if not br.error else None,
                "mean_tokens_per_burst": round(br.mean_tokens) if not br.error else None,
                "total_tokens": br.total_tokens if not br.error else None,
                "mean_s_per_burst": round(br.mean_s, 3) if not br.error else None,
            }
            for br in burst_results
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
    parser.add_argument(
        "--burst-size",
        "-k",
        type=int,
        default=5,
        metavar="K",
        help=f"files changed per burst (default: 5, max: {len(_BURST_FILES)})",
    )
    args = parser.parse_args()
    n, warmup = args.iterations, args.warmup
    burst_size = min(args.burst_size, len(_BURST_FILES))

    env = _env_info()

    with tempfile.TemporaryDirectory(prefix="pulci_bench_") as tmpdir:
        project_dir = pathlib.Path(tmpdir) / "fixture"
        shutil.copytree(FIXTURE_DIR, project_dir)
        touch_target = project_dir / _TOUCH_RELATIVE
        stats = _fixture_stats(project_dir)

        _print_header(env, stats, n, warmup)

        results: list[BenchResult] = []

        print(f"[1/3] manual   — warmup {warmup}... ", end="", flush=True)
        bench_manual(project_dir, warmup)
        print(f"measuring {n}... ", end="", flush=True)
        results.append(bench_manual(project_dir, n))
        print("done")

        print(f"[2/3] pulci    — starting daemon, warmup {warmup}... ", end="", flush=True)
        results.append(bench_pulci(project_dir, touch_target, n, warmup))
        print("done")

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
        print_latency_table(results)
        print_token_table(results)
        print_summary(results)

        # Burst section — isolated fixture copy to avoid state bleed from steady-state modes
        burst_dir = pathlib.Path(tmpdir) / "burst_fixture"
        shutil.copytree(FIXTURE_DIR, burst_dir)
        burst_files = [burst_dir / f for f in _BURST_FILES]
        burst_contents = [f.read_text() for f in burst_files]
        burst_results: list[BurstResult] = []

        print(f"\n[burst-1/2] manual   — {n} bursts of K={burst_size}... ", end="", flush=True)
        burst_results.append(
            bench_burst_manual(burst_dir, burst_files, burst_contents, n, burst_size)
        )
        print("done")

        print(
            f"[burst-2/2] pulci    — starting daemon, warmup {warmup}... ",
            end="",
            flush=True,
        )
        burst_results.append(
            bench_burst_pulci(burst_dir, burst_files, burst_contents, n, burst_size, warmup)
        )
        print("done")

        print_burst_table(burst_results)
        print_burst_summary(burst_results)

        out_path = pathlib.Path("benchmarks/results.json")
        out_path.parent.mkdir(exist_ok=True)
        summary = build_json_summary(results, burst_results, n, env, stats)
        out_path.write_text(json.dumps(summary, indent=2))
        print(f"\n  JSON → {out_path}\n")


if __name__ == "__main__":
    main()
