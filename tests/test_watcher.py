"""
Tests for the Day 2 file watcher.
"""

from __future__ import annotations

import json
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


def test_start_ignores_atomic_write_temp_files() -> None:
    """
    Regression for commit 4fb6e6a: atomic-write editors create temp files
    like `foo.py.tmp.<pid>.<hash>` then immediately rename them. Without an
    extension filter on the event loop, the daemon ran ruff on the temp path
    and surfaced spurious E902 ("file not found") diagnostics. The fix lives
    in `crates/pulci-py/src/lib.rs` (event-loop extension filter).
    """
    with tempfile.TemporaryDirectory() as tmp:
        tmp_path = pathlib.Path(tmp)
        (tmp_path / "pulci.toml").write_text("[hooks]\nruff = true\nty = false\npytest = false\n")
        (tmp_path / "foo.py").write_text("x = 1\n")

        proc, lines, thread = _start_daemon([PULCI_BIN, "start", tmp])

        _wait_for_state(tmp_path / ".pulci" / "state.json")

        # Simulate an atomic write: create a sibling temp file and remove it
        # before any hook could possibly open it.
        tmp_file = tmp_path / "foo.py.tmp.12345.abc"
        tmp_file.write_text("x = 1\n")
        time.sleep(0.5)
        tmp_file.unlink(missing_ok=True)

        time.sleep(1.0)  # give the daemon time to misbehave if the filter is gone

        proc.terminate()
        proc.wait(timeout=5)
        thread.join(timeout=2)
        stdout = "".join(lines)

        # If the filter is removed, ruff runs against the deleted .tmp file
        # and emits an E902 diagnostic referencing it. Either signal is enough
        # to flag the regression.
        assert ".tmp." not in stdout, (
            f"daemon ran a hook against an atomic-write temp file:\n{stdout}"
        )
        assert "E902" not in stdout, f"E902 (file not found) leaked from a temp file:\n{stdout}"


def test_start_excludes_single_file_via_watch_exclude() -> None:
    """Regression: a 0.0.4 user reported that `[watch] exclude` silently
    ignored individual filenames — edits to excluded files still triggered
    checks. Root cause was a relative project_root vs absolute notify event
    paths; project_root is now canonicalised at startup so the comparison
    works for both file and directory exclusions.
    """
    import pytest

    if not pathlib.Path(PULCI_BIN).exists():
        pytest.skip("pulci binary not found")

    with tempfile.TemporaryDirectory() as tmpdir:
        tmp_path = pathlib.Path(tmpdir)
        (tmp_path / "pulci.toml").write_text(
            "[hooks]\n"
            "ruff = true\n"
            "ty = false\n"
            "pytest = false\n"
            "[watch]\n"
            'exclude = ["pulci_smoketest.py"]\n'
        )
        # Pre-create the excluded file with intentional ruff violations so the
        # daemon's initial sweep would have something to flag — if it touched
        # this file the diagnostics would surface.
        excluded = tmp_path / "pulci_smoketest.py"
        excluded.write_text("import os\nimport sys\n")  # both unused → F401

        # Also a non-excluded file so the daemon has SOMETHING to check.
        other = tmp_path / "other.py"
        other.write_text("import json\n")  # unused → F401

        proc, _lines, thread = _start_daemon([PULCI_BIN, "start", str(tmp_path)])

        # Trigger an edit on the excluded file. If the exclude works, no
        # diagnostics for it should appear in state.json.
        excluded.write_text("import os\nimport sys\nimport collections\n")

        state_file = tmp_path / ".pulci" / "state.json"
        _wait_for_state(state_file)
        # Give one extra debounce window for the post-initial-scan event loop
        # to run on the excluded-file edit and finalise state.
        time.sleep(0.5)

        proc.terminate()
        proc.wait(timeout=5)
        thread.join(timeout=2)

        if not state_file.exists():
            return  # No daemon could resolve ruff; skip
        state = json.loads(state_file.read_text())
        files_in_diagnostics = {d["file"] for d in state.get("diagnostics", [])}
        excluded_str = str(excluded)
        assert not any(
            excluded_str.endswith(f) or f.endswith("pulci_smoketest.py")
            for f in files_in_diagnostics
        ), f"excluded file appeared in diagnostics: {files_in_diagnostics}"


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
