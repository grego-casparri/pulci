"""
Integration test for the single-instance daemon lock.

A second `pulci start` against a project that already has a running daemon
must fail fast with an actionable message rather than racing on state.json
and the heartbeat file.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys
import tempfile
import threading
import time

PULCI_BIN = str(pathlib.Path(sys.executable).parent / "pulci")


def _start_daemon(cmd: list[str], ready_marker: str = "Watching", timeout: float = 15.0):
    proc = subprocess.Popen(
        cmd,
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


def test_second_pulci_start_fails_with_lock_message() -> None:
    """
    Spawn two `pulci start <tmp>` processes on the same project. The first
    must reach the "Watching" ready line. The second must exit non-zero with
    a message that mentions both the lock file and "already running".
    """
    import pytest

    if not pathlib.Path(PULCI_BIN).exists():
        pytest.skip("pulci binary not found")

    with tempfile.TemporaryDirectory() as tmp:
        project = pathlib.Path(tmp).resolve()
        (project / "pulci.toml").write_text("[hooks]\nruff = true\nty = false\npytest = false\n")

        first, _first_lines, first_thread = _start_daemon(
            [PULCI_BIN, "start", str(project)],
        )
        try:
            # Give the first daemon a moment to acquire the advisory lock.
            time.sleep(0.5)
            second = subprocess.run(
                [PULCI_BIN, "start", str(project)],
                capture_output=True,
                text=True,
                timeout=15,
            )
        finally:
            first.terminate()
            try:
                first.wait(timeout=5)
            except subprocess.TimeoutExpired:
                first.kill()
                first.wait(timeout=5)
            first_thread.join(timeout=2)

        combined = (second.stdout or "") + (second.stderr or "")
        assert second.returncode != 0, (
            f"second daemon exited 0 — lock not enforced.\n"
            f"stdout:\n{second.stdout}\nstderr:\n{second.stderr}"
        )
        assert "another pulci daemon is already running" in combined, (
            f"expected lock message, got:\nstdout:{second.stdout}\nstderr:{second.stderr}"
        )
        assert "daemon.lock" in combined, (
            f"expected lock path in message, got:\nstdout:{second.stdout}\nstderr:{second.stderr}"
        )
