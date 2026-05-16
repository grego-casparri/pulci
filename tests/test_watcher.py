"""
Tests for the Day 2 file watcher.
"""

from __future__ import annotations

import pathlib
import subprocess
import sys
import tempfile
import time

from pulci.cli import app
from typer.testing import CliRunner

runner = CliRunner()

# The installed pulci script lives alongside the Python executable in the venv.
PULCI_BIN = str(pathlib.Path(sys.executable).parent / "pulci")


def test_start_help_exits_cleanly() -> None:
    result = runner.invoke(app, ["start", "--help"])
    assert result.exit_code == 0
    assert "PATH" in result.output


def test_start_detects_file_creation() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        proc = subprocess.Popen(
            [PULCI_BIN, "start", tmp],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        time.sleep(0.3)

        new_file = pathlib.Path(tmp) / "hello.py"
        new_file.write_text("x = 1\n")

        time.sleep(0.5)
        proc.terminate()
        stdout, _ = proc.communicate(timeout=5)

        # The new output format emits a summary line like "0 errors, 0 warnings (N files checked)"
        assert "errors" in stdout


def test_start_ignores_pycache() -> None:
    with tempfile.TemporaryDirectory() as tmp:
        pycache = pathlib.Path(tmp) / "__pycache__"
        pycache.mkdir()

        proc = subprocess.Popen(
            [PULCI_BIN, "start", tmp],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        time.sleep(0.3)

        (pycache / "mod.pyc").write_bytes(b"\x00" * 16)

        time.sleep(0.5)
        proc.terminate()
        stdout, _ = proc.communicate(timeout=5)

        assert "__pycache__" not in stdout


def test_start_agent_mode_emits_compiler_style_summary() -> None:
    """
    --agent mode now emits compiler-style diagnostics (same as human mode),
    not JSON events. Verify the summary line is present after a check.
    """
    import pytest

    if not pathlib.Path(PULCI_BIN).exists():
        pytest.skip("pulci binary not found")

    with tempfile.TemporaryDirectory() as tmpdir:
        target = pathlib.Path(tmpdir) / "foo.py"
        target.write_text("import os\n")
        (pathlib.Path(tmpdir) / "pulci.toml").write_text(
            "[hooks]\nruff = true\nty = false\npytest = false\n"
        )

        proc = subprocess.Popen(
            [PULCI_BIN, "start", "--agent", tmpdir],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        time.sleep(1.0)
        target.write_text("import os\nimport sys\n")
        time.sleep(1.0)
        proc.terminate()
        stdout, _ = proc.communicate(timeout=5)

    # Expect the compiler-style summary line: "N errors, N warnings (N files checked)"
    assert "errors" in stdout, f"expected summary line in stdout, got: {stdout!r}"
    assert "warnings" in stdout, f"expected summary line in stdout, got: {stdout!r}"
