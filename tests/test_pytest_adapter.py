"""
Regression tests for the pytest hook adapter.

These tests guard against bugs that have shipped to main without coverage.
Each test docstring names the commit it backfills.
"""

from __future__ import annotations

import json
import os
import pathlib
import re
import subprocess
import sys
import tempfile
import threading
import time

PULCI_BIN = str(pathlib.Path(sys.executable).parent / "pulci")

_FOOTER_RE = re.compile(r"\d+ errors?, \d+ warnings? \(\d+ files? checked")


def _start_daemon(
    cmd: list[str],
    cwd: str,
    ready_marker: str = "Watching",
    timeout: float = 15.0,
):
    """
    Spawn `cmd` with `cwd`, return (proc, lines, thread) once ready_marker is seen.
    """
    proc = subprocess.Popen(
        cmd,
        cwd=cwd,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
    )
    lines: list[str] = []
    ready = threading.Event()

    def _reader() -> None:
        assert proc.stdout is not None
        for line in proc.stdout:
            lines.append(line)
            if ready_marker in line:
                ready.set()

    thread = threading.Thread(target=_reader, daemon=True)
    thread.start()
    ready.wait(timeout=timeout)
    return proc, lines, thread


def _wait_for_state(state_path: pathlib.Path, timeout: float = 15.0) -> bool:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if state_path.exists():
            return True
        time.sleep(0.1)
    return False


def _wait_for_pytest_diagnostic(state_path: pathlib.Path, timeout: float = 15.0) -> dict | None:
    """
    Poll state.json until a pytest diagnostic appears or timeout expires.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if state_path.exists():
            try:
                state = json.loads(state_path.read_text())
            except json.JSONDecodeError:
                time.sleep(0.05)
                continue
            for d in state.get("diagnostics", []):
                if d.get("tool") == "pytest":
                    return state
        time.sleep(0.1)
    return None


def test_pytest_adapter_runs_from_different_cwd() -> None:
    """
    Regression for commit 843d470: PytestAdapter used to invoke pytest from
    the daemon process cwd instead of the project root. When the daemon was
    started from a directory other than the watched project, pytest could not
    find the relative test paths and silently produced no diagnostics.

    The fix lives in `crates/pulci-core/src/hooks/pytest.rs`
    (`Command::current_dir(&self.project_root)`).

    This test spawns the daemon from `cwd != project_root`, edits a Python
    source file whose corresponding test fails, and asserts that the pytest
    diagnostic surfaces in state.json. Reverting the `current_dir` line drops
    the diagnostic (pytest is launched from the wrong directory and either
    fails to collect or returns no results), causing this test to fail.
    """
    import pytest

    if not pathlib.Path(PULCI_BIN).exists():
        pytest.skip("pulci binary not found")

    with tempfile.TemporaryDirectory() as project_tmp, tempfile.TemporaryDirectory() as launch_tmp:
        project = pathlib.Path(project_tmp).resolve()
        launch_cwd = pathlib.Path(launch_tmp).resolve()
        assert project != launch_cwd

        (project / "pulci.toml").write_text("[hooks]\nruff = false\nty = false\npytest = true\n")

        src_dir = project / "src"
        src_dir.mkdir()
        (src_dir / "__init__.py").write_text("")
        target = src_dir / "calc.py"
        target.write_text("def add(a, b):\n    return a + b\n")

        tests_dir = project / "tests"
        tests_dir.mkdir()
        (tests_dir / "test_calc.py").write_text(
            "from src.calc import add\n\n"
            "def test_add_intentional_failure():\n"
            "    assert add(1, 1) == 999\n"
        )

        env = os.environ.copy()
        # Make sure src/ is importable when pytest collects from project_root.
        env["PYTHONPATH"] = str(project)

        proc = subprocess.Popen(
            [PULCI_BIN, "start", str(project)],
            cwd=str(launch_cwd),
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            env=env,
        )
        lines: list[str] = []
        ready = threading.Event()

        def _reader() -> None:
            assert proc.stdout is not None
            for line in proc.stdout:
                lines.append(line)
                if "Watching" in line:
                    ready.set()

        thread = threading.Thread(target=_reader, daemon=True)
        thread.start()
        try:
            ready.wait(timeout=20.0)

            # Touch the source file to trigger the watcher event loop.
            time.sleep(0.3)
            target.write_text("def add(a, b):\n    return a + b  # bump\n")

            state = _wait_for_pytest_diagnostic(project / ".pulci" / "state.json", timeout=20.0)
        finally:
            proc.terminate()
            try:
                proc.wait(timeout=5)
            except subprocess.TimeoutExpired:
                proc.kill()
                proc.wait(timeout=5)
            thread.join(timeout=2)

        stdout = "".join(lines)
        assert state is not None, (
            "no pytest diagnostic in state.json — adapter did not run "
            f"from project_root.\nDaemon output:\n{stdout}"
        )
        pytest_diags = [d for d in state["diagnostics"] if d["tool"] == "pytest"]
        assert any("test_add_intentional_failure" in d["message"] for d in pytest_diags), (
            f"expected failure diagnostic missing; got: {pytest_diags!r}"
        )
