"""
Tests for the Day 2 file watcher.
"""

from __future__ import annotations

import pathlib
import re
import subprocess
import sys
import tempfile
import time

from pulci.cli import app
from typer.testing import CliRunner

runner = CliRunner()

# The installed pulci script lives alongside the Python executable in the venv.
PULCI_BIN = str(pathlib.Path(sys.executable).parent / "pulci")

# Footer pattern per FORMATS.md: "N errors, M warnings (K files checked, T.Xs)"
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


def test_start_help_exits_cleanly() -> None:
    result = runner.invoke(app, ["start", "--help"])
    assert result.exit_code == 0
    assert "PATH" in result.output


def test_start_detects_file_creation() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = pathlib.Path(tmp)
        # Only ruff — avoids uvx cold-start for ty in CI, keeps the test fast.
        (tmp_path / "pulci.toml").write_text("[hooks]\nruff = true\nty = false\npytest = false\n")
        proc = subprocess.Popen(
            [PULCI_BIN, "start", tmp],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        time.sleep(0.5)  # allow daemon to start and register the watcher

        new_file = tmp_path / "hello.py"
        new_file.write_text("x = 1\n")

        state_appeared = _wait_for_state(tmp_path / ".pulci" / "state.json")
        proc.terminate()
        stdout, _ = proc.communicate(timeout=5)

        assert state_appeared, "state.json never appeared — check ran?"
        assert _FOOTER_RE.search(stdout), (
            f"expected footer matching {_FOOTER_RE.pattern!r}, got: {stdout!r}"
        )


def test_start_ignores_pycache() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = pathlib.Path(tmp)
        pycache = tmp_path / "__pycache__"
        pycache.mkdir()

        proc = subprocess.Popen(
            [PULCI_BIN, "start", tmp],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        time.sleep(0.3)

        (pycache / "mod.pyc").write_bytes(b"\x00" * 16)

        time.sleep(0.8)  # long enough for a check to fire if the event leaked
        proc.terminate()
        stdout, _ = proc.communicate(timeout=5)

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

        proc = subprocess.Popen(
            [PULCI_BIN, "start", "--agent", tmpdir],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        time.sleep(0.5)
        target.write_text("import os\nimport sys\n")

        _wait_for_state(tmp_path / ".pulci" / "state.json")
        proc.terminate()
        stdout, _ = proc.communicate(timeout=5)

    assert _FOOTER_RE.search(stdout), (
        f"expected footer matching {_FOOTER_RE.pattern!r}, got: {stdout!r}"
    )
