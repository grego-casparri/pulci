"""
MCP server for pulci — exposes pulci_status tool via stdio.

Run with: pulci mcp
"""

from __future__ import annotations

import json
import pathlib
import sys

from mcp.server.fastmcp import FastMCP

mcp = FastMCP(
    "pulci",
    instructions=(
        "pulci is a continuous quality gate daemon for Python projects. "
        "Use pulci_status to get the current state of ruff, ty, and pytest "
        "checks after each file edit. Never invoke ruff/ty/pytest directly "
        "while pulci is active — it already ran them in the background."
    ),
)


@mcp.tool()
def pulci_status(path: str = ".") -> dict:
    """
    Return the current quality gate state for the project at path.

    Returns a state object with schema_version, summary, tools, and diagnostics.
    If the daemon is not running, returns {"status": "not_running", "hint": "..."}.
    """
    state_file = pathlib.Path(path).resolve() / ".pulci" / "state.json"
    if not state_file.exists():
        return {
            "status": "not_running",
            "hint": f"run `pulci start {path}` in your terminal to start the daemon",
        }
    try:
        return json.loads(state_file.read_text())
    except json.JSONDecodeError:
        return {
            "status": "error",
            "hint": "state.json is corrupted — stop and restart `pulci start`",
        }


def print_mcp_info(path: str = ".") -> None:
    """
    Print the MCP config block the user pastes into their host config.
    """
    pulci_bin = str(pathlib.Path(sys.executable).parent / "pulci")
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
