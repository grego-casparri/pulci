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
_HEARTBEAT_DEAD_SECS = 120


def _daemon_dead(state_dir: pathlib.Path) -> bool:
    """
    Return True if the daemon heartbeat is absent or older than _HEARTBEAT_DEAD_SECS.
    """
    import datetime

    hb_file = state_dir / "heartbeat"
    if not hb_file.exists():
        return True
    try:
        ts_str = hb_file.read_text().strip()
        ts = datetime.datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
        secs_ago = (datetime.datetime.now(datetime.timezone.utc) - ts).total_seconds()
        return secs_ago >= _HEARTBEAT_DEAD_SECS
    except Exception:
        return True


def _read_state(state_file: pathlib.Path) -> dict:
    try:
        return json.loads(state_file.read_text())
    except json.JSONDecodeError:
        return {
            "status": "error",
            "hint": "state.json is corrupted — stop and restart `pulci start`",
        }


@mcp.tool()
async def pulci_status(
    path: str = ".",
    wait_for_file: str | None = None,
    since_version: int | None = None,
    timeout_ms: int = 5000,
) -> dict:
    """
    Return the current quality gate state for the project at path.

    Returns a state object with schema_version, state_version, summary,
    tools, and diagnostics.

    If the daemon is not running, returns {"status": "not_running", "hint": "..."}.

    Causal synchronisation (D-013):
      - since_version: block until state_version > since_version.
      - wait_for_file: semantic hint for which file you care about. The daemon
        produces global state (not per-file), so the actual wait is driven by
        since_version. Always pair wait_for_file with since_version.
      - timeout_ms: max wait in milliseconds (default 5000). Returns
        {"status": "timeout"} if the daemon does not produce a new result in time.

    Recommended agent loop:
      v = (await pulci_status()).get("state_version", 0)
      # edit foo.py
      result = await pulci_status(wait_for_file="foo.py", since_version=v)
    """
    state_file = pathlib.Path(path).resolve() / ".pulci" / "state.json"
    state_dir = state_file.parent

    _not_running = {
        "status": "not_running",
        "hint": f"run `pulci start {path}` in your terminal to start the daemon",
    }

    if not state_file.exists():
        return _not_running

    # Fast path — no blocking requested.
    if since_version is None:
        return _read_state(state_file)

    # If daemon is confirmed dead, no point waiting for a version that will never arrive.
    if _daemon_dead(state_dir):
        return _not_running

    # Blocking path — wait until state_version > since_version.
    deadline = time.monotonic() + timeout_ms / 1000.0

    while True:
        if not state_file.exists() or _daemon_dead(state_dir):
            return _not_running
        state = _read_state(state_file)
        current_version = state.get("state_version", 0)
        if current_version > since_version:
            return state

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
    args: list[str] = ["mcp"]
    if path != ".":
        args.append(path)
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
