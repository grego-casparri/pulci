"""
Command-line interface for pulci.
"""

from __future__ import annotations

import json
import pathlib
from typing import Annotated, Literal

import typer

from pulci import __version__, _native
from pulci.mcp_server import mcp as _mcp_server
from pulci.mcp_server import print_mcp_info


def _version_callback(value: bool) -> None:
    if value:
        typer.echo(__version__)
        raise typer.Exit


app = typer.Typer(
    name="pulci",
    help="Continuous quality gate daemon for agent-driven Python development.",
    no_args_is_help=True,
    add_completion=False,
)

_mcp_app = typer.Typer(
    name="mcp",
    help="Run the pulci MCP server for Claude Desktop, Cursor, and compatible hosts.",
    invoke_without_command=True,
    no_args_is_help=False,
)
app.add_typer(_mcp_app)


@app.callback()
def _main(
    version: Annotated[
        bool,
        typer.Option(
            "--version",
            "-V",
            callback=_version_callback,
            is_eager=True,
            help="Show pulci version and exit.",
        ),
    ] = False,
) -> None:
    """
    pulci entrypoint.
    """


@app.command()
def start(
    path: Annotated[str, typer.Argument(help="Project root to watch.")] = ".",
    agent: Annotated[
        bool,
        typer.Option(
            "--agent",
            help="Emit compact JSON events per check instead of human text.",
        ),
    ] = False,
) -> None:
    """
    Watch PATH for changes, run quality hooks, and update .pulci/state.json.

    Reads hook configuration from pulci.toml in the project root if present.
    Press Ctrl-C to stop.
    """
    try:
        _native.start(path, agent)
    except KeyboardInterrupt:
        if agent:
            typer.echo('{"event":"stopped"}')
        else:
            typer.echo("Stopped.")
    except Exception as exc:
        if agent:
            typer.echo(json.dumps({"event": "error", "message": str(exc)}))
        else:
            typer.echo(f"Error: {exc}", err=True)
        raise typer.Exit(code=1) from exc


@app.command()
def status(
    path: Annotated[str, typer.Argument(help="Project root to read state from.")] = ".",
    json_output: Annotated[
        bool,
        typer.Option("--json", help="Emit agent-mode JSON instead of human output."),
    ] = False,
) -> None:
    """
    Show current quality gate state. Reads .pulci/state.json written by `pulci start`.
    """
    state_file = pathlib.Path(path) / ".pulci" / "state.json"

    if not state_file.exists():
        if json_output:
            typer.echo(json.dumps({"status": "not_running", "hint": "run `pulci start` first"}))
        else:
            typer.echo("No state available. Run `pulci start` first.", err=True)
        raise typer.Exit(code=0)

    raw = state_file.read_text()

    try:
        state = json.loads(raw)
    except json.JSONDecodeError as exc:
        if json_output:
            typer.echo(json.dumps({"error": f"corrupted state file: {exc}"}))
        else:
            typer.echo(f"State file is corrupted: {exc}", err=True)
        raise typer.Exit(code=2) from exc

    summary = state.get("summary", {})

    if json_output:
        typer.echo(raw)
        if summary.get("errors", 0) > 0:
            raise typer.Exit(code=1)
        return

    tools = state.get("tools", [])
    if tools:
        typer.echo("Tools:")
        for t in tools:
            name = t.get("name", "")
            version = t.get("version", "unknown")
            source = t.get("source", "")
            path = t.get("path") or ""
            typer.echo(f"  {name:<8} {version:<8} {source:<12} {path}")
        typer.echo("")

    typer.echo(f"  errors    {summary.get('errors', 0)}")
    typer.echo(f"  warnings  {summary.get('warnings', 0)}")
    typer.echo(f"  checks    {summary.get('checks_run', 0)}")
    typer.echo(f"  updated   {state.get('timestamp', 'unknown')}")
    typer.echo(f"  stale     {summary.get('stale', False)}")

    diagnostics = state.get("diagnostics", [])
    if diagnostics:
        typer.echo("\nDiagnostics:")
        for d in diagnostics:
            file_ = d.get("file", "")
            line = d.get("line", 0)
            col = d.get("col", 0)
            code = d.get("code") or ""
            sev = d.get("severity", "error")
            msg = d.get("message", "")
            tool = d.get("tool", "")
            code_part = f"[{tool}/{code}]" if code else f"[{tool}]"
            typer.echo(f"  {file_}:{line}:{col}: {sev}{code_part} {msg}")

    if summary.get("errors", 0) > 0:
        raise typer.Exit(code=1)


@_mcp_app.callback()
def mcp_cmd(
    ctx: typer.Context,
    path: Annotated[str, typer.Argument(help="Project root to serve state from.")] = ".",
    transport: Annotated[
        Literal["stdio"],
        typer.Option("--transport", help="MCP transport protocol."),
    ] = "stdio",
) -> None:
    """
    Start the pulci MCP server (stdio).

    Exposes the pulci_status tool to any MCP-compatible host
    (Claude Desktop, Cursor, Claude Code). Run `pulci mcp info` to get
    the config block to paste into your host.
    """
    if ctx.invoked_subcommand is None:
        import os

        if path != ".":
            os.chdir(path)
        typer.echo(
            f"Starting pulci MCP server (stdio, project: {pathlib.Path(path).resolve()}).",
            err=True,
        )
        typer.echo("Run `pulci mcp info` to get the config block for your MCP host.", err=True)
        _mcp_server.run(transport=transport)


@_mcp_app.command("info")
def mcp_info(
    path: Annotated[str, typer.Argument(help="Project root (embedded in config).")] = ".",
) -> None:
    """
    Print the MCP config block to paste into Claude Desktop or Cursor.
    """
    print_mcp_info(path)


if __name__ == "__main__":
    app()
