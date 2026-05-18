"""
Command-line interface for pulci.
"""

from __future__ import annotations

import json
import pathlib
from typing import Annotated, Literal

import typer

from pulci import __version__, _native
from pulci._heartbeat import (
    heartbeat_info as _heartbeat_info,
)
from pulci._heartbeat import (
    last_check_seconds_ago as _last_check_seconds_ago,
)
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
            help="Suppress startup messages. Structured exit events emitted on stop or error.",
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
def doctor(
    path: Annotated[str, typer.Argument(help="Project root to diagnose.")] = ".",
    json_output: Annotated[
        bool,
        typer.Option("--json", help="Emit structured JSON instead of human output."),
    ] = False,
) -> None:
    """
    Self-diagnosis: project root, pulci.toml validity, tool resolution
    for every enabled hook, .pulci/ writability, daemon status, state.json
    integrity. Exits 0 when everything passes, 1 if any check fails. Run
    this when "pulci start" surfaces a confusing error — it tells you which
    layer is broken without running the daemon.
    """
    from pulci.doctor import diagnose, render_human

    report = diagnose(pathlib.Path(path))
    if json_output:
        typer.echo(json.dumps(report.to_json(), indent=2))
    else:
        typer.echo(render_human(report))
    raise typer.Exit(code=report.exit_code())


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
        from pulci._heartbeat import read_startup_error as _read_startup_error

        hb = _heartbeat_info(state_file.parent)
        if hb["daemon_status"] == "alive":
            payload = {
                "status": "running_no_checks_yet",
                "daemon_status": "alive",
                "hint": "daemon is running — touch any .py file to trigger the first check",
            }
            if json_output:
                typer.echo(json.dumps(payload))
            else:
                typer.echo("Daemon is running. Touch any .py file to trigger the first check.")
        else:
            # Enrich with startup_error.json so a config/pin failure is
            # surfaced to the user instead of generic "run pulci start" —
            # they probably DID run it and it bailed before writing state.
            startup_err = _read_startup_error(state_file.parent)
            hint = "run `pulci start` first"
            if startup_err:
                hint = (
                    f"pulci start failed: {startup_err.get('message', 'unknown error')}. "
                    f"Fix the issue and re-run `pulci start`."
                )
            if json_output:
                response: dict = {"status": "not_running", "hint": hint}
                if startup_err:
                    response["startup_error"] = startup_err
                typer.echo(json.dumps(response))
            else:
                if startup_err:
                    typer.echo(f"pulci start failed: {startup_err.get('message')}", err=True)
                    typer.echo("  Fix the issue and re-run `pulci start`.", err=True)
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
    hb = _heartbeat_info(state_file.parent)
    last_check_secs = _last_check_seconds_ago(state)

    if json_output:
        state["daemon_status"] = hb["daemon_status"]
        if hb["daemon_heartbeat_at"] is not None:
            state["daemon_heartbeat_at"] = hb["daemon_heartbeat_at"]
        age: dict = {}
        if hb["heartbeat_seconds_ago"] is not None:
            age["heartbeat_seconds_ago"] = hb["heartbeat_seconds_ago"]
        if last_check_secs is not None:
            age["last_check_seconds_ago"] = last_check_secs
        if age:
            state["age"] = age
        typer.echo(json.dumps(state))
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

    daemon_status = hb["daemon_status"]
    hb_secs = hb["heartbeat_seconds_ago"]
    if daemon_status == "alive":
        typer.echo(f"  daemon    alive (heartbeat {hb_secs}s ago)")
    elif daemon_status == "stale_heartbeat":
        typer.echo(f"  daemon    stale (heartbeat {hb_secs}s ago — may be processing)")
    else:
        typer.echo("  daemon    dead — run 'pulci start' to restart")
        if last_check_secs is not None:
            typer.echo(
                f"            last check {last_check_secs}s ago — diagnostics may be outdated"
            )

    typer.echo(f"  errors    {summary.get('errors', 0)}")
    typer.echo(f"  warnings  {summary.get('warnings', 0)}")
    typer.echo(f"  checks    {summary.get('checks_run', 0)}")
    if last_check_secs is not None:
        typer.echo(f"  last check {last_check_secs}s ago")
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

    tool_errors = state.get("tool_errors", [])
    if tool_errors:
        typer.echo("\nTool errors (hooks that produced no verdict):")
        for te in tool_errors:
            typer.echo(f"  {te.get('tool', '?')}: {te.get('message', '')}")

    if summary.get("errors", 0) > 0:
        raise typer.Exit(code=1)


@_mcp_app.callback()
def mcp_cmd(
    ctx: typer.Context,
    path: Annotated[
        str,
        typer.Option(
            "--path",
            help="Project root to serve state from. Defaults to the current directory.",
        ),
    ] = ".",
    transport: Annotated[
        Literal["stdio"],
        typer.Option("--transport", help="MCP transport protocol."),
    ] = "stdio",
) -> None:
    """
    Start the pulci MCP server (stdio).

    Exposes the pulci_status tool to any MCP-compatible host
    (Claude Desktop, Cursor, Claude Code). Run `pulci mcp info` to get
    the config block to paste into your host, or `pulci mcp install <host>`
    to write the entry automatically.
    """
    # `path` is an option (not a positional) so it does not shadow subcommand
    # names — `pulci mcp info` and `pulci mcp install <host>` must remain
    # routable to their commands.
    if ctx.invoked_subcommand is None:
        import os

        if path != ".":
            os.chdir(path)
        # Stdio is the only transport today and the only consumer is an MCP
        # host that's literally treating our stdout/stderr as the wire. Any
        # courtesy text we add here ends up as noise in the host's logs.
        # Skip the chatty banner; setup-time messages belong in
        # `pulci mcp info` / `pulci mcp install`, not in the live server.
        _mcp_server.run(transport=transport)


@_mcp_app.command("info")
def mcp_info(
    path: Annotated[str, typer.Argument(help="Project root (embedded in config).")] = ".",
) -> None:
    """
    Print the MCP config block to paste into Claude Desktop or Cursor.
    """
    print_mcp_info(path)


@_mcp_app.command("install")
def mcp_install(
    host: Annotated[
        str,
        typer.Argument(
            help=(
                "Target host: claude-desktop or cursor. "
                "For Claude Code, use `claude mcp add` directly."
            ),
        ),
    ],
    path: Annotated[
        str,
        typer.Argument(help="Project root pulci will watch (embedded in the args)."),
    ] = ".",
    global_scope: Annotated[
        bool,
        typer.Option(
            "--global",
            help="Cursor only: install into ~/.cursor/mcp.json instead of .cursor/mcp.json.",
        ),
    ] = False,
    dry_run: Annotated[
        bool,
        typer.Option(
            "--dry-run",
            help="Print the config that would be written; don't touch disk.",
        ),
    ] = False,
) -> None:
    """
    Install pulci into a supported MCP host's config file.

    Atomic-write (tmp + rename) preserves any existing `mcpServers` entries.
    Refuses to overwrite a config file that is not valid JSON — fix the file
    manually first.
    """
    from pulci.mcp_install import install

    try:
        result = install(
            host,
            project_path=path,
            global_scope=global_scope,
            dry_run=dry_run,
        )
    except (ValueError, RuntimeError) as exc:
        typer.echo(f"pulci mcp install: {exc}", err=True)
        raise typer.Exit(code=1) from exc

    config_path = result["path"]
    if dry_run:
        typer.echo(f"Would write to: {config_path}")
        typer.echo("")
        typer.echo(json.dumps(result["payload"], indent=2))
        return

    verb = "updated" if result["was_present"] else "installed"
    typer.echo(f"pulci {verb} in {host} config:")
    typer.echo(f"  {config_path}")
    typer.echo("")
    typer.echo(f"Restart {host} (or reload the MCP server list) to pick up the change.")


_KNOWN_MCP_SUBCOMMANDS = frozenset({"info", "install"})


def _rewrite_legacy_mcp_args() -> None:
    """Backward compat shim: `pulci mcp <PATH>` → `pulci mcp --path <PATH>`.

    The 0.0.5 fix for `pulci mcp info` crashing on startup moved the
    project-root argument from a positional to a `--path` flag (without it,
    typer ate `info` and `install` as positional values and crashed before
    dispatching the subcommand). The change broke any caller that already
    passed a positional path — including MCP hosts whose configs had
    `args: ["mcp", "/some/project"]`. Detect that pattern and rewrite to the
    new form with a one-line deprecation warning, so existing integrations
    keep working through the 0.0.x line. Remove this shim in 0.1.0.
    """
    import sys

    if len(sys.argv) >= 3 and sys.argv[1] == "mcp":
        third = sys.argv[2]
        # Only rewrite if it doesn't look like a flag AND isn't a subcommand
        # we already know. New invocations using `--path` or subcommands are
        # passed through untouched.
        if not third.startswith("-") and third not in _KNOWN_MCP_SUBCOMMANDS:
            sys.stderr.write(
                f"pulci: warning: `pulci mcp {third}` is deprecated; "
                f"use `pulci mcp --path {third}`. This compatibility shim "
                f"will be removed in 0.1.0.\n"
            )
            sys.argv[2:3] = ["--path", third]


def main() -> None:
    """
    Entry point. Runs the legacy-args shim before handing off to typer.
    """
    _rewrite_legacy_mcp_args()
    app()


if __name__ == "__main__":
    main()
