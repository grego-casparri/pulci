"""
Tests for the Day 4 status command and state file.
"""

from __future__ import annotations

import json
import pathlib
import re

from pulci.cli import app
from typer.testing import CliRunner

# Compiler-style diagnostic line per FORMATS.md:
# <file>:<line>:<col>: <severity>[<tool>/<code>] <message>
_DIAGNOSTIC_RE = re.compile(r"[\w./][\w./-]*:\d+:\d+: \w+\[\w+/\w+\] .+")

runner = CliRunner()

MINIMAL_STATE = {
    "schema_version": 1,
    "state_version": 1,
    "timestamp": "2026-05-16T01:00:00Z",
    "summary": {"errors": 1, "warnings": 0, "checks_run": 2, "stale": False},
    "diagnostics": [
        {
            "tool": "ruff",
            "file": "src/main.py",
            "line": 5,
            "col": 0,
            "severity": "error",
            "code": "F401",
            "message": "`os` imported but unused",
        }
    ],
}


def write_state(cwd: pathlib.Path, state: dict) -> None:
    state_file = cwd / ".pulci" / "state.json"
    state_file.parent.mkdir(parents=True, exist_ok=True)
    state_file.write_text(json.dumps(state))


def test_status_no_state_file_exits_zero_with_hint() -> None:
    # No daemon running → not an error condition; exits 0 with a hint message.
    with runner.isolated_filesystem():
        result = runner.invoke(app, ["status"])
    assert result.exit_code == 0


def test_status_human_output() -> None:
    with runner.isolated_filesystem() as tmp:
        write_state(pathlib.Path(tmp), MINIMAL_STATE)
        result = runner.invoke(app, ["status"])
    assert result.exit_code == 1  # errors > 0 → exit 1
    assert "errors" in result.output
    assert "1" in result.output
    assert "F401" in result.output
    assert "src/main.py" in result.output
    # Verify compiler-style format: file:line:col: severity[tool/code] message
    assert _DIAGNOSTIC_RE.search(result.output), (
        f"diagnostic line does not match compiler format, got:\n{result.output}"
    )


def test_status_json_output_is_valid_json() -> None:
    with runner.isolated_filesystem() as tmp:
        write_state(pathlib.Path(tmp), MINIMAL_STATE)
        result = runner.invoke(app, ["status", "--json"])
    assert result.exit_code == 1  # errors > 0 → exit 1, even in --json mode
    parsed = json.loads(result.output)
    assert parsed["schema_version"] == 1
    assert parsed["summary"]["errors"] == 1


def test_status_json_schema_fields_present() -> None:
    with runner.isolated_filesystem() as tmp:
        write_state(pathlib.Path(tmp), MINIMAL_STATE)
        result = runner.invoke(app, ["status", "--json"])
    assert result.exit_code == 1  # errors > 0
    parsed = json.loads(result.output)
    assert "schema_version" in parsed
    assert "timestamp" in parsed
    assert "summary" in parsed
    assert "diagnostics" in parsed


def test_status_corrupted_state_file() -> None:
    with runner.isolated_filesystem() as tmp:
        state_dir = pathlib.Path(tmp) / ".pulci"
        state_dir.mkdir()
        (state_dir / "state.json").write_text("{ this is not valid json }")
        result = runner.invoke(app, ["status"])
    assert result.exit_code == 2
    assert "corrupted" in result.output.lower()


def test_status_corrupted_state_file_json() -> None:
    with runner.isolated_filesystem() as tmp:
        state_dir = pathlib.Path(tmp) / ".pulci"
        state_dir.mkdir()
        (state_dir / "state.json").write_text("not json at all")
        result = runner.invoke(app, ["status", "--json"])
    assert result.exit_code == 2
    data = json.loads(result.output.strip())
    assert "error" in data
