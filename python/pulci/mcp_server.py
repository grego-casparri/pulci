"""
MCP server for pulci — exposes pulci_status tool via stdio.

Run with: pulci mcp
"""

from __future__ import annotations

import asyncio
import json
import pathlib
import shutil
import sys
import time

from mcp.server.fastmcp import FastMCP

from pulci._heartbeat import (
    daemon_dead as _daemon_dead,
)
from pulci._heartbeat import (
    enrich as _enrich,
)
from pulci._heartbeat import (
    heartbeat_info as _heartbeat_info,
)
from pulci._heartbeat import (
    read_startup_error as _read_startup_error,
)
from pulci._heartbeat import (
    read_state_json as _read_state,
)


def _not_running_response(path: str, state_dir: pathlib.Path) -> dict:
    """
    Build a `{"status": "not_running"}` response, enriched with the
    startup-error file when the daemon failed to start. Without the
    enrichment, an agent watching pulci_status sees "not_running" both
    when no daemon was launched AND when one was launched but bailed on a
    bad pulci.toml — and retries blindly in the second case.
    """
    base: dict = {
        "status": "not_running",
        "hint": f"run `pulci start {path}` in your terminal to start the daemon",
    }
    err = _read_startup_error(state_dir)
    if err:
        base["startup_error"] = err
        base["hint"] = (
            f"pulci start failed: {err.get('message', 'unknown error')}. "
            f"Fix the issue and re-run `pulci start {path}`."
        )
    return base


mcp = FastMCP(
    "pulci",
    instructions=(
        "pulci is a continuous quality gate daemon for Python projects. "
        "Use pulci_status to get the current state of ruff, ty, and pytest "
        "checks after each file edit. Never invoke ruff/ty/pytest directly "
        "while pulci is active — it already ran them in the background. "
        "For causal synchronisation after an edit, pass since_version so the "
        "tool blocks until a fresh result is ready."
    ),
)

_POLL_INTERVAL = 0.05  # seconds between state.json reads when waiting


@mcp.tool()
async def pulci_status(
    path: str = ".",
    since_version: int | None = None,
    timeout_ms: int = 5000,
) -> dict:
    """
    Return the current quality gate state for the project at path.

    Returns a state object with schema_version, state_version, summary,
    tools, and diagnostics.

    If the daemon is not running, returns {"status": "not_running", "hint": "..."}.

    Causal synchronisation:
      - since_version: block until state_version > since_version. Used by
        agents to wait for a fresh result after editing a file: capture the
        current version before editing, then call with since_version=that.
      - timeout_ms: max wait in milliseconds (default 5000). Returns
        {"status": "timeout"} if the daemon does not produce a new result in time.

    Recommended agent loop:
      v = (await pulci_status()).get("state_version", 0)
      # edit foo.py
      result = await pulci_status(since_version=v)
    """
    state_file = pathlib.Path(path).resolve() / ".pulci" / "state.json"
    state_dir = state_file.parent

    if not state_file.exists():
        # Parity with `pulci status`: distinguish "no daemon at all" from
        # "daemon just started, initial scan still running". Without this the
        # CLI and MCP gave different answers for the same daemon state.
        hb = _heartbeat_info(state_dir)
        if hb["daemon_status"] == "alive":
            return {
                "status": "running_no_checks_yet",
                "daemon_status": "alive",
                "hint": "daemon is running — initial scan in progress or no file changes yet",
            }
        return _not_running_response(path, state_dir)

    # Fast path — no blocking requested.
    if since_version is None:
        return _enrich(_read_state(state_file), state_dir)

    # If daemon is confirmed dead, no point waiting for a version that will never arrive.
    if _daemon_dead(state_dir):
        return _not_running_response(path, state_dir)

    # Blocking path — wait until state_version > since_version.
    deadline = time.monotonic() + timeout_ms / 1000.0

    while True:
        if not state_file.exists() or _daemon_dead(state_dir):
            return _not_running_response(path, state_dir)
        state = _read_state(state_file)
        current_version = state.get("state_version", 0)
        if current_version > since_version:
            return _enrich(state, state_dir)

        remaining = deadline - time.monotonic()
        if remaining <= 0:
            return {
                "status": "timeout",
                "hint": (
                    "The daemon did not produce a new result within "
                    f"{timeout_ms}ms. Is `pulci start` running?"
                ),
            }
        await asyncio.sleep(min(_POLL_INTERVAL, remaining))


def print_mcp_info(path: str = ".") -> None:
    """
    Print the MCP config block the user pastes into their host config.
    """
    pulci_bin = shutil.which("pulci") or str(pathlib.Path(sys.executable).parent / "pulci")
    # `--path` rather than a bare positional: the mcp subcommand layer (info,
    # install) needs subcommand names to NOT be shadowed by a positional path
    # arg. Configs that omit a custom path stay as `args: ["mcp"]`.
    args: list[str] = ["mcp"]
    if path != ".":
        args.extend(["--path", path])
    config = {
        "mcpServers": {
            "pulci": {
                "command": pulci_bin,
                "args": args,
            }
        }
    }
    print(json.dumps(config, indent=2))
    print()
    print("Paste the above into:")
    print("  Claude Desktop: ~/Library/Application Support/Claude/claude_desktop_config.json")
    print("  Cursor:         .cursor/mcp.json  (project) or ~/.cursor/mcp.json (global)")
