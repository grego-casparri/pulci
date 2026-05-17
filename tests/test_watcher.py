"""
Tests for the Day 2 file watcher.
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys
import tempfile
import threading
import time

from pulci.cli import app
from typer.testing import CliRunner

runner = CliRunner()

PULCI_BIN = str(pathlib.Path(sys.executable).parent / "pulci")

_FOOTER_RE = re.compile(r"\d+ errors?, \d+ warnings? \(\d+ files? checked")


def _wait_for_state(state_path: pathlib.Path, timeout: float = 8.0) -> bool:
    """
    Poll until state.json appears or timeout expires.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if state_path.exists():
            return True
        time.sleep(0.1)
    return False


def _start_daemon(cmd: list[str], ready_marker: str = "Watching", timeout: float = 10.0):
    """
    Start the daemon and wait until its stdout contains ready_marker.

    Returns (proc, lines_list) where lines_list is a shared list that the
    background reader thread appends to. After proc.terminate() + proc.wait(),
    join the reader thread and ''.join(lines_list) is the full stdout.
    """
    proc = subprocess.Popen(
        cmd,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
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


def test_start_help_exits_cleanly() -> None:
    result = runner.invoke(app, ["start", "--help"])
    assert result.exit_code == 0
    assert "PATH" in result.output


def test_start_detects_file_creation() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = pathlib.Path(tmp)
        (tmp_path / "pulci.toml").write_text("[hooks]\nruff = true\nty = false\npytest = false\n")
        proc, lines, thread = _start_daemon([PULCI_BIN, "start", tmp])

        new_file = tmp_path / "hello.py"
        new_file.write_text("x = 1\n")

        state_appeared = _wait_for_state(tmp_path / ".pulci" / "state.json")
        proc.terminate()
        proc.wait(timeout=5)
        thread.join(timeout=2)
        stdout = "".join(lines)

        assert state_appeared, "state.json never appeared — check ran?"
        assert _FOOTER_RE.search(stdout), (
            f"expected footer matching {_FOOTER_RE.pattern!r}, got: {stdout!r}"
        )


def test_start_ignores_pycache() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = pathlib.Path(tmp)
        pycache = tmp_path / "__pycache__"
        pycache.mkdir()

        proc, lines, thread = _start_daemon([PULCI_BIN, "start", tmp])

        (pycache / "mod.pyc").write_bytes(b"\x00" * 16)

        time.sleep(0.8)  # wait long enough for a check to fire if the event leaked
        proc.terminate()
        proc.wait(timeout=5)
        thread.join(timeout=2)
        stdout = "".join(lines)

        assert "__pycache__" not in stdout


def test_start_agent_mode_emits_compiler_style_summary() -> None:
    """
    --agent mode emits compiler-style diagnostics per FORMATS.md.
    Verify the footer matches the grammar exactly.
    """
    import pytest

    if not pathlib.Path(PULCI_BIN).exists():
        pytest.skip("pulci binary not found")

    with tempfile.TemporaryDirectory() as tmpdir:
        tmp_path = pathlib.Path(tmpdir)
        target = tmp_path / "foo.py"
        target.write_text("import os\n")
        (tmp_path / "pulci.toml").write_text("[hooks]\nruff = true\nty = false\npytest = false\n")

        proc, lines, thread = _start_daemon([PULCI_BIN, "start", "--agent", tmpdir])
        target.write_text("import os\nimport sys\n")

        _wait_for_state(tmp_path / ".pulci" / "state.json")
        proc.terminate()
        proc.wait(timeout=5)
        thread.join(timeout=2)
        stdout = "".join(lines)

    assert _FOOTER_RE.search(stdout), (
        f"expected footer matching {_FOOTER_RE.pattern!r}, got: {stdout!r}"
    )
