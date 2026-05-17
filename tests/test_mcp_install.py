"""
Tests for `pulci mcp install` — the auto-installer that writes the pulci
entry into a supported MCP host's config file. The destination paths
are OS-specific in production, so every test uses the `config_path_override`
hook to write to a tempdir; OS-detection is exercised indirectly by the
resolver tests further down.
"""

from __future__ import annotations

import json
import pathlib

import pytest
from pulci.mcp_install import (
    SUPPORTED_HOSTS,
    install,
    merge_pulci_entry,
    resolve_config_path,
)


def test_install_writes_pulci_entry_into_empty_file(tmp_path: pathlib.Path) -> None:
    target = tmp_path / "claude_desktop_config.json"
    result = install(
        "claude-desktop",
        config_path_override=target,
    )
    assert target.exists()
    assert not result["was_present"]
    data = json.loads(target.read_text())
    assert "pulci" in data["mcpServers"]
    assert data["mcpServers"]["pulci"]["args"] == ["mcp"]


def test_install_preserves_existing_mcp_servers(tmp_path: pathlib.Path) -> None:
    # A user who already has other MCP servers configured must not lose them.
    target = tmp_path / "claude_desktop_config.json"
    existing = {
        "mcpServers": {
            "filesystem": {"command": "/usr/local/bin/mcp-fs", "args": ["--root", "."]},
            "github": {"command": "mcp-github", "args": []},
        }
    }
    target.write_text(json.dumps(existing, indent=2))

    install("claude-desktop", config_path_override=target)

    data = json.loads(target.read_text())
    assert set(data["mcpServers"].keys()) == {"filesystem", "github", "pulci"}
    assert data["mcpServers"]["filesystem"]["args"] == ["--root", "."]


def test_install_updates_existing_pulci_entry(tmp_path: pathlib.Path) -> None:
    # A re-install replaces the entry (so the binary path refreshes), and
    # the result flags `was_present=True` so the CLI can phrase the message
    # as "updated" rather than "installed".
    target = tmp_path / "claude_desktop_config.json"
    target.write_text(
        json.dumps({"mcpServers": {"pulci": {"command": "/old/path/pulci", "args": ["mcp"]}}})
    )
    result = install("claude-desktop", config_path_override=target)
    assert result["was_present"]
    data = json.loads(target.read_text())
    assert data["mcpServers"]["pulci"]["command"] != "/old/path/pulci"


def test_install_writes_path_argument_when_non_default(tmp_path: pathlib.Path) -> None:
    target = tmp_path / "config.json"
    install(
        "cursor",
        project_path="/path/to/project",
        config_path_override=target,
    )
    data = json.loads(target.read_text())
    # Non-default paths are passed as `--path <value>` (not a bare positional)
    # so the mcp subcommand layer (info, install) doesn't get shadowed.
    assert data["mcpServers"]["pulci"]["args"] == ["mcp", "--path", "/path/to/project"]


def test_install_dry_run_does_not_touch_disk(tmp_path: pathlib.Path) -> None:
    target = tmp_path / "config.json"
    result = install(
        "cursor",
        config_path_override=target,
        dry_run=True,
    )
    assert result["dry_run"] is True
    assert not target.exists(), "dry-run should not create the config file"


def test_install_refuses_to_overwrite_malformed_json(tmp_path: pathlib.Path) -> None:
    # If the existing file is unreadable JSON, refuse rather than silently
    # discard the user's content.
    target = tmp_path / "config.json"
    target.write_text("{ not valid json")
    with pytest.raises(RuntimeError, match="not valid JSON"):
        install("claude-desktop", config_path_override=target)
    # Original content must be untouched.
    assert target.read_text() == "{ not valid json"


def test_install_creates_parent_directories(tmp_path: pathlib.Path) -> None:
    # First-time install on a fresh machine: ~/.cursor/ may not exist yet.
    target = tmp_path / "deep" / "nested" / "dir" / "config.json"
    install("cursor", config_path_override=target)
    assert target.exists()


def test_install_rejects_unknown_host() -> None:
    with pytest.raises(ValueError, match="unknown host"):
        install("definitely-not-a-host", config_path_override=pathlib.Path("/tmp/x"))


def test_install_rejects_global_for_claude_desktop(tmp_path: pathlib.Path) -> None:
    with pytest.raises(ValueError, match="only meaningful for `cursor`"):
        install(
            "claude-desktop",
            global_scope=True,
            config_path_override=tmp_path / "x.json",
        )


def test_install_rejects_nonobject_mcp_servers_field(tmp_path: pathlib.Path) -> None:
    # A user whose config has `mcpServers: "something"` (wrong type) — refuse
    # to obliterate it.
    target = tmp_path / "config.json"
    target.write_text(json.dumps({"mcpServers": "weird"}))
    with pytest.raises(RuntimeError, match="not an object"):
        install("claude-desktop", config_path_override=target)


def test_merge_pulci_entry_is_pure() -> None:
    existing = {"mcpServers": {"other": {"command": "x", "args": []}}}
    merged, was_present = merge_pulci_entry(existing, ".")
    assert not was_present
    # Existing dict untouched (function does not mutate input).
    assert "pulci" not in existing["mcpServers"]
    assert "pulci" in merged["mcpServers"]
    assert merged["mcpServers"]["other"] == existing["mcpServers"]["other"]


def test_resolve_config_path_cursor_project_local(tmp_path: pathlib.Path) -> None:
    path = resolve_config_path(
        "cursor",
        project_root=tmp_path,
        global_scope=False,
    )
    assert path == tmp_path.resolve() / ".cursor" / "mcp.json"


def test_resolve_config_path_cursor_global_lives_in_home() -> None:
    path = resolve_config_path(
        "cursor",
        project_root=pathlib.Path(),
        global_scope=True,
    )
    # Path lives under the user home directory regardless of cwd.
    assert path == pathlib.Path.home() / ".cursor" / "mcp.json"


def test_supported_hosts_list() -> None:
    # Sanity guard: the public catalogue covers both claude-desktop and cursor.
    assert "claude-desktop" in SUPPORTED_HOSTS
    assert "cursor" in SUPPORTED_HOSTS
