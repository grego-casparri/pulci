"""
Heartbeat reading and daemon health derivation.

Shared between `cli.py` (for `pulci status` output enrichment) and
`mcp_server.py` (for `pulci_status` MCP tool responses) so the two
surfaces report `daemon_status` consistently. The daemon writes
`.pulci/heartbeat` every 10 s; both consumers derive
alive/stale_heartbeat/dead from the file's age.
"""

from __future__ import annotations

import datetime
import json
import pathlib
import sys

HEARTBEAT_ALIVE_SECS = 30
HEARTBEAT_DEAD_SECS = 120


def heartbeat_info(state_dir: pathlib.Path) -> dict:
    """
    Read `.pulci/heartbeat` and derive daemon health.

    Returns a dict with keys `daemon_status`, `daemon_heartbeat_at`,
    `heartbeat_seconds_ago`. On any I/O or parse failure the function
    returns the dead state with a one-line warning on stderr so the
    cause is debuggable.
    """
    hb_file = state_dir / "heartbeat"
    if not hb_file.exists():
        return {
            "daemon_status": "dead",
            "daemon_heartbeat_at": None,
            "heartbeat_seconds_ago": None,
        }
    try:
        ts_str = hb_file.read_text().strip()
        ts = datetime.datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
    except (OSError, ValueError) as exc:
        print(f"pulci: warning: heartbeat read failed: {exc}", file=sys.stderr)
        return {
            "daemon_status": "dead",
            "daemon_heartbeat_at": None,
            "heartbeat_seconds_ago": None,
        }
    now = datetime.datetime.now(datetime.timezone.utc)
    secs_ago = int((now - ts).total_seconds())
    if secs_ago < HEARTBEAT_ALIVE_SECS:
        status = "alive"
    elif secs_ago < HEARTBEAT_DEAD_SECS:
        status = "stale_heartbeat"
    else:
        status = "dead"
    return {
        "daemon_status": status,
        "daemon_heartbeat_at": ts_str,
        "heartbeat_seconds_ago": secs_ago,
    }


def daemon_dead(state_dir: pathlib.Path) -> bool:
    """
    True if the daemon heartbeat is absent or older than HEARTBEAT_DEAD_SECS.
    """
    return heartbeat_info(state_dir)["daemon_status"] == "dead"


def last_check_seconds_ago(state: dict) -> int | None:
    """
    Derive how many seconds elapsed since the state's `timestamp` field.
    Returns None when the timestamp is missing or unparseable (logged).
    """
    ts_str = state.get("timestamp")
    if not ts_str:
        return None
    try:
        ts = datetime.datetime.fromisoformat(ts_str.replace("Z", "+00:00"))
    except (TypeError, ValueError) as exc:
        print(f"pulci: warning: state.timestamp unparseable: {exc}", file=sys.stderr)
        return None
    return int((datetime.datetime.now(datetime.timezone.utc) - ts).total_seconds())


def enrich(state: dict, state_dir: pathlib.Path) -> dict:
    """
    Augment a state dict with the CLI-injected fields documented in
    `schemas/state.v1.schema.json`: `daemon_status`, `daemon_heartbeat_at`,
    and `age` (with `heartbeat_seconds_ago` and `last_check_seconds_ago`).
    """
    hb = heartbeat_info(state_dir)
    state["daemon_status"] = hb["daemon_status"]
    if hb["daemon_heartbeat_at"] is not None:
        state["daemon_heartbeat_at"] = hb["daemon_heartbeat_at"]
    age: dict = {}
    if hb["heartbeat_seconds_ago"] is not None:
        age["heartbeat_seconds_ago"] = hb["heartbeat_seconds_ago"]
    last_check = last_check_seconds_ago(state)
    if last_check is not None:
        age["last_check_seconds_ago"] = last_check
    if age:
        state["age"] = age
    return state


def read_state_json(state_file: pathlib.Path) -> dict:
    """
    Read and parse `.pulci/state.json`. Returns the parsed dict, or a
    structured `{"status": "error", ...}` envelope when the file is
    corrupted. I/O errors are propagated to the caller (they can
    distinguish "file gone" from "file unreadable" via os.errno).
    """
    try:
        return json.loads(state_file.read_text())
    except json.JSONDecodeError as exc:
        print(f"pulci: warning: state.json corrupted: {exc}", file=sys.stderr)
        return {
            "status": "error",
            "hint": "state.json is corrupted — stop and restart `pulci start`",
        }
