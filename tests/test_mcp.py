"""
Tests for the pulci MCP server tool and info helper.
"""

from __future__ import annotations

import asyncio
import datetime
import json
import pathlib
import threading
import time

import pytest
from pulci.mcp_server import print_mcp_info, pulci_status


def _run(coro):
    """
    Run a coroutine synchronously (no pytest-asyncio dependency).
    """
    return asyncio.run(coro)


def _write_state(state_dir: pathlib.Path, state: dict) -> None:
    state_dir.mkdir(parents=True, exist_ok=True)
    (state_dir / "state.json").write_text(json.dumps(state))


def _write_heartbeat(state_dir: pathlib.Path, secs_ago: int = 5) -> None:
    """
    Write a fresh heartbeat so the daemon appears alive.
    """
    state_dir.mkdir(parents=True, exist_ok=True)
    ts = datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(seconds=secs_ago)
    (state_dir / "heartbeat").write_text(ts.strftime("%Y-%m-%dT%H:%M:%SZ"))


MINIMAL_STATE = {
    "schema_version": 1,
    "state_version": 1,
    "timestamp": "2026-05-17T10:00:00Z",
    "summary": {"errors": 0, "warnings": 0, "checks_run": 2, "stale": False},
    "tools": [],
    "diagnostics": [],
}


def test_pulci_status_no_daemon(tmp_path: pathlib.Path) -> None:
    result = _run(pulci_status(str(tmp_path)))
    assert result["status"] == "not_running"
    assert "hint" in result


def test_pulci_status_returns_state(tmp_path: pathlib.Path) -> None:
    _write_state(tmp_path / ".pulci", MINIMAL_STATE)
    _write_heartbeat(tmp_path / ".pulci")
    result = _run(pulci_status(str(tmp_path)))
    assert result["schema_version"] == 1
    assert result["state_version"] == 1
    assert result["summary"]["errors"] == 0


def test_pulci_status_dead_daemon_fast_path_returns_last_state(tmp_path: pathlib.Path) -> None:
    # Fast path (no since_version): dead daemon still returns last known state.
    # Blocking path is where dead daemon → not_running (see test_wait_for_* tests).
    _write_state(tmp_path / ".pulci", MINIMAL_STATE)
    _write_heartbeat(tmp_path / ".pulci", secs_ago=200)
    result = _run(pulci_status(str(tmp_path)))
    assert result["schema_version"] == 1


def test_pulci_status_corrupted_state(tmp_path: pathlib.Path) -> None:
    state_dir = tmp_path / ".pulci"
    state_dir.mkdir()
    (state_dir / "state.json").write_text("{ not valid json")
    _write_heartbeat(state_dir)

    result = _run(pulci_status(str(tmp_path)))
    assert result["status"] == "error"
    assert "hint" in result


def test_mcp_info_prints_valid_json(tmp_path: pathlib.Path, capsys: pytest.CaptureFixture) -> None:
    print_mcp_info(str(tmp_path))
    captured = capsys.readouterr()
    config = json.loads(captured.out.split("\n\n")[0])
    assert "mcpServers" in config
    assert "pulci" in config["mcpServers"]
    server = config["mcpServers"]["pulci"]
    assert "command" in server
    assert "args" in server
    assert "mcp" in server["args"]


def test_mcp_info_default_path_omits_dot(capsys: pytest.CaptureFixture) -> None:
    print_mcp_info(".")
    captured = capsys.readouterr()
    first_block = captured.out[: captured.out.find("\n\n")]
    config = json.loads(first_block)
    args = config["mcpServers"]["pulci"]["args"]
    assert args == ["mcp"], f"default path should not appear in args, got {args}"


def test_mcp_info_nondefault_path_included(
    tmp_path: pathlib.Path, capsys: pytest.CaptureFixture
) -> None:
    print_mcp_info(str(tmp_path))
    captured = capsys.readouterr()
    first_block = captured.out[: captured.out.find("\n\n")]
    config = json.loads(first_block)
    args = config["mcpServers"]["pulci"]["args"]
    assert str(tmp_path) in args


def test_mcp_info_command_is_pulci_bin(capsys: pytest.CaptureFixture) -> None:
    print_mcp_info(".")
    captured = capsys.readouterr()
    first_block = captured.out[: captured.out.find("\n\n")]
    config = json.loads(first_block)
    command = config["mcpServers"]["pulci"]["command"]
    # Command must resolve to the pulci binary (stem covers pulci.EXE on Windows).
    assert pathlib.Path(command).stem == "pulci"


def test_wait_for_file_without_since_version_is_fast_path(tmp_path: pathlib.Path) -> None:
    # wait_for_file alone (no since_version) must return immediately — fast path.
    _write_state(tmp_path / ".pulci", MINIMAL_STATE)
    _write_heartbeat(tmp_path / ".pulci")
    result = _run(pulci_status(str(tmp_path), wait_for_file="foo.py"))
    assert result["schema_version"] == 1


def test_wait_for_returns_immediately_when_version_advanced(tmp_path: pathlib.Path) -> None:
    state = {**MINIMAL_STATE, "state_version": 5}
    _write_state(tmp_path / ".pulci", state)
    _write_heartbeat(tmp_path / ".pulci")

    result = _run(pulci_status(str(tmp_path), since_version=3, timeout_ms=1000))
    assert result.get("state_version") == 5


def test_wait_for_times_out_when_version_not_advanced(tmp_path: pathlib.Path) -> None:
    state = {**MINIMAL_STATE, "state_version": 2}
    _write_state(tmp_path / ".pulci", state)
    _write_heartbeat(tmp_path / ".pulci")

    # since_version == current version → no new result → timeout
    result = _run(pulci_status(str(tmp_path), since_version=2, timeout_ms=200))
    assert result["status"] == "timeout"
    assert "hint" in result


def test_wait_for_detects_version_bump_mid_wait(tmp_path: pathlib.Path) -> None:
    state_dir = tmp_path / ".pulci"
    _write_state(state_dir, {**MINIMAL_STATE, "state_version": 1})
    _write_heartbeat(state_dir)

    def bump_after_delay() -> None:
        time.sleep(0.15)
        _write_state(state_dir, {**MINIMAL_STATE, "state_version": 2})
        _write_heartbeat(state_dir)

    t = threading.Thread(target=bump_after_delay, daemon=True)
    t.start()

    result = _run(pulci_status(str(tmp_path), since_version=1, timeout_ms=2000))
    t.join(timeout=1)

    assert result.get("state_version") == 2


def test_wait_for_no_daemon_during_wait(tmp_path: pathlib.Path) -> None:
    # State file missing from the start with wait params → not_running
    result = _run(pulci_status(str(tmp_path), since_version=0, timeout_ms=200))
    assert result["status"] == "not_running"
