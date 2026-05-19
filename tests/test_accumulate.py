"""
End-to-end tests for Q-16: state.json accumulates per-file diagnostics.

Validates that editing one file does NOT erase the diagnostics from other
files (the bug Q-16 fixes), and that deletes + restarts behave correctly.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import time

PULCI_BIN = str(pathlib.Path(sys.executable).parent / "pulci")


def _wait_for_state(state_file: pathlib.Path, timeout_s: float = 15.0) -> dict | None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if state_file.exists():
            try:
                return json.loads(state_file.read_text())
            except json.JSONDecodeError:
                pass
        time.sleep(0.05)
    return None


def _wait_for_version(
    state_file: pathlib.Path, min_version: int, timeout_s: float = 10.0
) -> dict | None:
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        try:
            data = json.loads(state_file.read_text())
            if data.get("state_version", 0) >= min_version:
                return data
        except (json.JSONDecodeError, FileNotFoundError):
            pass
        time.sleep(0.05)
    return None


def _spawn(tmp_path: pathlib.Path) -> subprocess.Popen:
    return subprocess.Popen(
        [PULCI_BIN, "start", str(tmp_path), "--agent"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def _terminate(proc: subprocess.Popen) -> None:
    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()
        proc.wait(timeout=2)


def test_accumulate_global_state(tmp_path: pathlib.Path) -> None:
    """
    Editing one file clean keeps the other files' diagnostics in state.json.
    """
    (tmp_path / "pulci.toml").write_text("[hooks]\nruff = true\nty = false\n")
    (tmp_path / "a.py").write_text("import os  # 1\n")
    (tmp_path / "b.py").write_text("import sys  # 2\n")
    (tmp_path / "c.py").write_text("import re  # 3\n")

    proc = _spawn(tmp_path)
    state_file = tmp_path / ".pulci" / "state.json"
    try:
        initial = _wait_for_state(state_file)
        assert initial is not None
        baseline_version = initial["state_version"]
        assert initial["summary"]["errors"] == 3
        diag_files_initial = {d["file"] for d in initial["diagnostics"]}
        assert len(diag_files_initial) == 3

        (tmp_path / "a.py").write_text("x = 1\n")
        after_edit = _wait_for_version(state_file, baseline_version + 1)
        assert after_edit is not None
        diag_files_after = {d["file"] for d in after_edit["diagnostics"]}
        assert str(tmp_path / "b.py") in diag_files_after
        assert str(tmp_path / "c.py") in diag_files_after
        assert after_edit["summary"]["errors"] == 2
    finally:
        _terminate(proc)


def test_accumulate_delete_removes_entry(tmp_path: pathlib.Path) -> None:
    """
    Deleting a file removes its diagnostics from state.json.
    """
    (tmp_path / "pulci.toml").write_text("[hooks]\nruff = true\nty = false\n")
    (tmp_path / "a.py").write_text("import os\n")
    (tmp_path / "b.py").write_text("import sys\n")
    (tmp_path / "c.py").write_text("import re\n")

    proc = _spawn(tmp_path)
    state_file = tmp_path / ".pulci" / "state.json"
    try:
        initial = _wait_for_state(state_file)
        assert initial is not None and initial["summary"]["errors"] == 3
        baseline_version = initial["state_version"]

        (tmp_path / "b.py").unlink()
        after_delete = _wait_for_version(state_file, baseline_version + 1)
        assert after_delete is not None
        diag_files_after = {d["file"] for d in after_delete["diagnostics"]}
        assert str(tmp_path / "b.py") not in diag_files_after
        assert after_delete["summary"]["errors"] == 2
    finally:
        _terminate(proc)


def test_accumulate_survives_restart(tmp_path: pathlib.Path) -> None:
    """
    State.json after restart still reflects the project.
    """
    (tmp_path / "pulci.toml").write_text("[hooks]\nruff = true\nty = false\n")
    (tmp_path / "a.py").write_text("import os\n")
    (tmp_path / "b.py").write_text("import sys\n")

    proc = _spawn(tmp_path)
    state_file = tmp_path / ".pulci" / "state.json"
    try:
        first = _wait_for_state(state_file)
        assert first is not None and first["summary"]["errors"] == 2
    finally:
        _terminate(proc)

    proc = _spawn(tmp_path)
    try:
        time.sleep(2)
        second = json.loads(state_file.read_text())
        diag_files = {d["file"] for d in second["diagnostics"]}
        assert str(tmp_path / "a.py") in diag_files
        assert str(tmp_path / "b.py") in diag_files
        assert second["summary"]["errors"] == 2
    finally:
        _terminate(proc)
