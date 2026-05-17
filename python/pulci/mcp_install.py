"""
Install the pulci MCP server entry into a supported host's config file.

Today's `pulci mcp info` prints a JSON config block the user pastes by
hand. `pulci mcp install <host>` does the same edit automatically:
locate the host's config file, merge the pulci entry into `mcpServers`,
and write atomically. Atomic write (tmp + rename) keeps a partial write
from ever corrupting the user's existing config.
"""

from __future__ import annotations

import json
import os
import pathlib
import shutil
import sys

# Supported hosts. Keep the catalogue small and well-tested; new hosts
# arrive when users actually ask.
SUPPORTED_HOSTS = ("claude-desktop", "cursor")


def _claude_desktop_config_path() -> pathlib.Path:
    """
    Per-OS location of Claude Desktop's config file. Anthropic publishes
    these paths in the official MCP setup docs.
    """
    if sys.platform == "darwin":
        return pathlib.Path.home() / "Library/Application Support/Claude/claude_desktop_config.json"
    if sys.platform == "win32":
        appdata = os.environ.get("APPDATA")
        if not appdata:
            raise RuntimeError("Could not locate Claude Desktop config: %APPDATA% is not set")
        return pathlib.Path(appdata) / "Claude" / "claude_desktop_config.json"
    # Linux + others
    return pathlib.Path.home() / ".config/Claude/claude_desktop_config.json"


def _cursor_config_path(*, project_root: pathlib.Path, global_scope: bool) -> pathlib.Path:
    """
    Cursor's MCP config. Project-scoped at `.cursor/mcp.json` by default;
    `--global` switches to `~/.cursor/mcp.json` (applies to every project).
    """
    if global_scope:
        return pathlib.Path.home() / ".cursor" / "mcp.json"
    return project_root.resolve() / ".cursor" / "mcp.json"


def resolve_config_path(
    host: str,
    *,
    project_root: pathlib.Path,
    global_scope: bool,
    override: pathlib.Path | None = None,
) -> pathlib.Path:
    """
    Compute the on-disk path for `host`'s MCP config. `override` short-circuits
    the host-specific resolution (useful for tests and custom layouts).
    """
    # Validate flag combinations BEFORE the override short-circuit so callers
    # that pass an explicit path still get the safety check.
    if host == "claude-desktop" and global_scope:
        raise ValueError("--global is only meaningful for `cursor`; Claude Desktop is user-scoped")
    if override is not None:
        return override
    if host == "claude-desktop":
        return _claude_desktop_config_path()
    if host == "cursor":
        return _cursor_config_path(project_root=project_root, global_scope=global_scope)
    raise ValueError(f"unknown host: {host!r}. Supported: {', '.join(SUPPORTED_HOSTS)}")


def _pulci_bin() -> str:
    """
    Resolve the path to the currently-active `pulci` executable. Matches
    `print_mcp_info` in `mcp_server.py` so the install path and the info-print
    path always agree on which binary lands in the config.
    """
    return shutil.which("pulci") or str(pathlib.Path(sys.executable).parent / "pulci")


def _build_pulci_entry(project_path: str) -> dict:
    """
    Construct the value that goes under `mcpServers.pulci` in the host config.
    Identical shape to `pulci mcp info`'s output: `--path` flag rather than
    a bare positional so the mcp subcommand layer keeps its subcommands
    reachable.
    """
    args: list[str] = ["mcp"]
    if project_path != ".":
        args.extend(["--path", project_path])
    return {"command": _pulci_bin(), "args": args}


def _load_existing_config(path: pathlib.Path) -> dict:
    """
    Read and parse the host config. Missing file → empty config. Malformed
    JSON refuses to proceed: better than silently overwriting work the user
    can't recover.
    """
    if not path.exists():
        return {}
    try:
        content = path.read_text()
    except OSError as exc:
        raise RuntimeError(f"could not read {path}: {exc}") from exc
    if not content.strip():
        return {}
    try:
        return json.loads(content)
    except json.JSONDecodeError as exc:
        raise RuntimeError(
            f"refusing to write: existing config at {path} is not valid JSON ({exc}). "
            f"Fix the file manually or remove it before re-running."
        ) from exc


def merge_pulci_entry(existing: dict, project_path: str) -> tuple[dict, bool]:
    """
    Insert (or replace) the `mcpServers.pulci` entry. Returns a fresh dict
    (deep-copied so the input is not mutated) and a flag indicating whether
    pulci was already present — so the caller can phrase the user-facing
    message as "installed" vs "updated". Other entries under `mcpServers`
    are preserved.
    """
    import copy

    config = copy.deepcopy(existing)
    servers = config.setdefault("mcpServers", {})
    if not isinstance(servers, dict):
        raise RuntimeError(
            "refusing to write: `mcpServers` in the existing config is not an object"
        )
    was_present = "pulci" in servers
    servers["pulci"] = _build_pulci_entry(project_path)
    return config, was_present


def _write_atomic(path: pathlib.Path, content: str) -> None:
    """
    Write `content` to `path` via tmp + rename so a partial write can never
    corrupt the user's existing config.
    """
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(content)
    tmp.replace(path)


def install(
    host: str,
    *,
    project_path: str = ".",
    global_scope: bool = False,
    dry_run: bool = False,
    config_path_override: pathlib.Path | None = None,
) -> dict:
    """
    Install the pulci MCP entry into `host`'s config. Returns a dict
    describing what was done so the CLI layer can print a precise message.

    Keys in the return value:
      - `path`: pathlib.Path of the config file touched
      - `was_present`: True if pulci was already in the config (we updated)
      - `dry_run`: True if no write happened (preview mode)
      - `payload`: the dict that would be (or was) written
    """
    if host not in SUPPORTED_HOSTS:
        raise ValueError(
            f"unknown host: {host!r}. Supported: {', '.join(SUPPORTED_HOSTS)}. "
            f"Claude Code: use `claude mcp add pulci <path-to-pulci> mcp` directly."
        )

    config_path = resolve_config_path(
        host,
        project_root=pathlib.Path(project_path),
        global_scope=global_scope,
        override=config_path_override,
    )
    existing = _load_existing_config(config_path)
    merged, was_present = merge_pulci_entry(existing, project_path)
    payload_text = json.dumps(merged, indent=2) + "\n"

    if not dry_run:
        _write_atomic(config_path, payload_text)

    return {
        "path": config_path,
        "was_present": was_present,
        "dry_run": dry_run,
        "payload": merged,
    }
