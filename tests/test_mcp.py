"""
Tests for the pulci MCP server tool and info helper.
"""

from __future__ import annotations

import json
import pathlib
import sys

import pytest
from pulci.mcp_server import print_mcp_info, pulci_status


def test_pulci_status_no_daemon(tmp_path: pathlib.Path) -> None:
    result = pulci_status(str(tmp_path))
    assert result["status"] == "not_running"
    assert "hint" in result


def test_pulci_status_returns_state(tmp_path: pathlib.Path) -> None:
    state = {
        "schema_version": 1,
        "timestamp": "2026-05-17T10:00:00Z",
        "summary": {"errors": 0, "warnings": 0, "checks_run": 2, "stale": False},
        "tools": [],
        "diagnostics": [],
    }
    state_dir = tmp_path / ".pulci"
    state_dir.mkdir()
    (state_dir / "state.json").write_text(json.dumps(state))

    result = pulci_status(str(tmp_path))
    assert result["schema_version"] == 1
    assert result["summary"]["errors"] == 0


def test_pulci_status_corrupted_state(tmp_path: pathlib.Path) -> None:
    state_dir = tmp_path / ".pulci"
    state_dir.mkdir()
    (state_dir / "state.json").write_text("{ not valid json")

    result = pulci_status(str(tmp_path))
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
    expected = str(pathlib.Path(sys.executable).parent / "pulci")
    assert command == expected
