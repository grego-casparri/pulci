"""
`pulci doctor` — self-diagnosis without starting the daemon.

Answers the question "is pulci set up correctly here?" by exercising the
same resolution chain `pulci start` uses, validating config, and probing
filesystem state. Output is either human-readable (default, demoable) or
JSON (`--json`) for agent consumption. Exit code is 0 if every check
passes, 1 otherwise.

The 4-level tool resolver here mirrors `pulci_core::resolver::resolve_tool`.
If the Rust resolver changes its precedence, this mirror needs to follow —
the regression surface lives in `tests/test_doctor.py` (resolver-precedence
tests) so drift is detectable.
"""

from __future__ import annotations

import json
import pathlib
import shutil
import subprocess
import sys
from dataclasses import asdict, dataclass, field
from typing import Literal

from pulci._heartbeat import heartbeat_info

CheckStatus = Literal["pass", "fail", "warn"]

# Tools pulci knows how to resolve. Other names should never reach doctor —
# they would be a configuration typo caught by deny_unknown_fields in
# HooksConfig.
_KNOWN_TOOLS = ("ruff", "ty", "pytest", "cargo")

# Known keys per pulci.toml section. The Rust loader has
# `#[serde(deny_unknown_fields)]` on every struct; this mirror catches the same
# typos earlier, with a friendlier message, without having to start the daemon.
# When the Rust side adds a new key, update the matching set here.
_KNOWN_SECTIONS = {"hooks", "tools", "watch"}
_KNOWN_KEYS_BY_SECTION: dict[str, set[str]] = {
    "hooks": {
        "ruff",
        "ruff_format",
        "ty",
        "pytest",
        "clippy",
        "timeout_secs",
        "pytest_test_patterns",
    },
    "tools": {"ruff", "ty", "pytest"},
    "watch": {"exclude"},
}


@dataclass
class Check:
    """
    One diagnostic line: section, name, status, human message, optional details.
    """

    section: str
    name: str
    status: CheckStatus
    message: str
    details: dict = field(default_factory=dict)


@dataclass
class Report:
    checks: list[Check] = field(default_factory=list)

    def add(self, check: Check) -> None:
        self.checks.append(check)

    def failed(self) -> list[Check]:
        return [c for c in self.checks if c.status == "fail"]

    def exit_code(self) -> int:
        return 1 if self.failed() else 0

    def to_json(self) -> dict:
        return {
            "checks": [asdict(c) for c in self.checks],
            "summary": {
                "total": len(self.checks),
                "passed": sum(1 for c in self.checks if c.status == "pass"),
                "warned": sum(1 for c in self.checks if c.status == "warn"),
                "failed": len(self.failed()),
            },
        }


def _read_pulci_toml(project_root: pathlib.Path) -> dict | None:
    """Load `pulci.toml` if present. Returns None when absent; raises
    `tomllib.TOMLDecodeError` on malformed TOML so the caller can surface it."""
    cfg_path = project_root / "pulci.toml"
    if not cfg_path.exists():
        return None
    try:
        import tomllib
    except ImportError:  # Python <3.11 fallback
        import tomli as tomllib  # type: ignore[no-redef]
    return tomllib.loads(cfg_path.read_text())


def _enabled_hooks(config: dict | None) -> dict[str, bool]:
    """Resolve which hooks are enabled, mirroring `HooksConfig::default`.

    Defaults: ruff=True, ty=True. Everything else off. Same as Rust side.
    """
    defaults = {
        "ruff": True,
        "ruff_format": False,
        "ty": True,
        "pytest": False,
        "clippy": False,
    }
    if config is None:
        return defaults
    hooks_section = config.get("hooks", {})
    return {k: hooks_section.get(k, default) for k, default in defaults.items()}


def _resolve_tool(name: str, project_root: pathlib.Path, pinned: str | None) -> dict:
    """Mirror of `pulci_core::resolver::resolve_tool`. Returns a result dict
    with at least `found` and either `version`/`source`/`path` (on success)
    or `error` (on failure)."""
    # Level 1: explicit version pin (only via uvx)
    if pinned:
        uvx_path = shutil.which("uvx")
        if uvx_path is None:
            return {
                "found": False,
                "error": (
                    f"`{name}` is pinned to {pinned} in pulci.toml but `uvx` "
                    "is not installed (install: `pip install uv`)"
                ),
            }
        invocation = [uvx_path, f"{name}@{pinned}"]
        result = subprocess.run([*invocation, "--version"], capture_output=True, text=True)
        if result.returncode != 0:
            stderr_line = result.stderr.strip().splitlines()[-1] if result.stderr.strip() else ""
            return {
                "found": False,
                "error": (
                    f"`uvx {name}@{pinned} --version` failed (exit {result.returncode}). "
                    f"Check the version exists on PyPI. {stderr_line}"
                ),
            }
        return {
            "found": True,
            "version": _parse_version(result.stdout),
            "source": "pinned",
            "path": None,
        }

    # Level 2: project-local venv
    if sys.platform == "win32":
        venv_bin = project_root / ".venv" / "Scripts" / f"{name}.exe"
    else:
        venv_bin = project_root / ".venv" / "bin" / name
    if venv_bin.is_file():
        return _probe_existing_binary(venv_bin, source="local-venv")

    # Level 3: system PATH
    on_path = shutil.which(name)
    if on_path:
        return _probe_existing_binary(pathlib.Path(on_path), source="system-path")

    # Level 4: uvx fallback (latest)
    uvx_path = shutil.which("uvx")
    if uvx_path is None:
        return {
            "found": False,
            "error": (
                f"`{name}` not found in `.venv`, `$PATH`, or via uvx. "
                f"Install uv (`pip install uv`) or {name} directly."
            ),
        }
    result = subprocess.run([uvx_path, name, "--version"], capture_output=True, text=True)
    if result.returncode != 0:
        return {
            "found": False,
            "error": f"`uvx {name} --version` failed (exit {result.returncode})",
        }
    return {
        "found": True,
        "version": _parse_version(result.stdout),
        "source": "uvx-latest",
        "path": None,
    }


def _probe_existing_binary(path: pathlib.Path, *, source: str) -> dict:
    result = subprocess.run([str(path), "--version"], capture_output=True, text=True)
    if result.returncode != 0:
        return {
            "found": False,
            "error": f"`{path} --version` failed (exit {result.returncode})",
        }
    return {
        "found": True,
        "version": _parse_version(result.stdout),
        "source": source,
        "path": str(path),
    }


def _parse_version(stdout: str) -> str:
    """Extract the last whitespace-separated token from the first stdout line.
    Mirrors `crate::resolver::detect_version` exactly."""
    first = stdout.strip().splitlines()
    if not first:
        return "unknown"
    parts = first[0].split()
    return parts[-1] if parts else "unknown"


# ---------------------------------------------------------------------------
# Individual checks. Each appends one or more Check rows to the Report.
# ---------------------------------------------------------------------------


def _check_project_root(report: Report, project_root: pathlib.Path) -> None:
    section = "Configuration"
    if not project_root.exists():
        report.add(Check(section, "project root", "fail", f"path does not exist: {project_root}"))
        return
    if not project_root.is_dir():
        report.add(Check(section, "project root", "fail", f"not a directory: {project_root}"))
        return
    report.add(Check(section, "project root", "pass", str(project_root.resolve())))


def _check_pulci_toml(report: Report, project_root: pathlib.Path) -> dict | None:
    section = "Configuration"
    cfg_path = project_root / "pulci.toml"
    if not cfg_path.exists():
        report.add(
            Check(
                section,
                "pulci.toml",
                "warn",
                "not present — using built-in defaults (ruff + ty enabled)",
            )
        )
        return None
    try:
        config = _read_pulci_toml(project_root)
    except Exception as exc:
        report.add(
            Check(
                section,
                "pulci.toml",
                "fail",
                f"malformed TOML: {exc}",
            )
        )
        return None

    # Surface unknown keys — the Rust loader rejects them, but doctor catches
    # them earlier with a friendlier message so the user can fix before
    # `pulci start` even runs.
    unknown_top = set((config or {}).keys()) - _KNOWN_SECTIONS
    if unknown_top:
        report.add(
            Check(
                section,
                "pulci.toml",
                "fail",
                f"unknown section(s) in pulci.toml: {', '.join(sorted(unknown_top))}",
            )
        )
        return config

    # Walk each known section and catch typos at the key level
    # (e.g. `clipy` inside [hooks]).
    typos: list[str] = []
    for s_name, known in _KNOWN_KEYS_BY_SECTION.items():
        section_data = (config or {}).get(s_name, {})
        if not isinstance(section_data, dict):
            continue
        unknown_keys = set(section_data.keys()) - known
        for k in sorted(unknown_keys):
            typos.append(f"[{s_name}] {k}")
    if typos:
        report.add(
            Check(
                section,
                "pulci.toml",
                "fail",
                f"unknown key(s) — typo? {'; '.join(typos)}",
                details={"unknown_keys": typos},
            )
        )
        return config

    enabled = [k for k, v in _enabled_hooks(config).items() if v]
    report.add(
        Check(
            section,
            "pulci.toml",
            "pass",
            f"valid; enabled hooks: {', '.join(enabled) if enabled else '(none)'}",
            details={"enabled": enabled},
        )
    )
    return config


def _check_tools(report: Report, project_root: pathlib.Path, config: dict | None) -> None:
    section = "Tool resolution"
    hooks = _enabled_hooks(config)
    tools_section = (config or {}).get("tools", {})

    # ruff and ruff_format share the same binary
    if hooks["ruff"] or hooks["ruff_format"]:
        _check_one_tool(report, section, "ruff", project_root, tools_section.get("ruff"))
    if hooks["ty"]:
        _check_one_tool(report, section, "ty", project_root, tools_section.get("ty"))
    if hooks["pytest"]:
        _check_one_tool(report, section, "pytest", project_root, tools_section.get("pytest"))
    if hooks["clippy"]:
        _check_one_tool(report, section, "cargo", project_root, None)


def _check_one_tool(
    report: Report,
    section: str,
    name: str,
    project_root: pathlib.Path,
    pinned: str | None,
) -> None:
    if name not in _KNOWN_TOOLS:
        report.add(Check(section, name, "fail", f"unknown tool: {name}"))
        return
    result = _resolve_tool(name, project_root, pinned)
    if not result["found"]:
        report.add(Check(section, name, "fail", result["error"]))
        return

    # Foreign-venv detection: when the binary came from $PATH and lives
    # outside the project root, an unrelated venv probably won the resolve
    # because of shell-activation leak. Works but worth flagging.
    status: CheckStatus = "pass"
    foreign_note = ""
    bin_path = result.get("path")
    if result.get("source") == "system-path" and bin_path is not None:
        try:
            resolved_root = project_root.resolve()
            resolved_bin = pathlib.Path(bin_path).resolve()
            if not str(resolved_bin).startswith(str(resolved_root)):
                status = "warn"
                foreign_note = "  ← outside project root"
        except OSError:
            pass

    location = bin_path or f"({result['source']})"
    message = f"{result['version']:<10} via {result['source']:<11} {location}{foreign_note}"
    report.add(
        Check(
            section,
            name,
            status,
            message.strip(),
            details={
                "version": result["version"],
                "source": result["source"],
                "path": result.get("path"),
                "outside_project_root": bool(foreign_note),
            },
        )
    )


def _check_pulci_dir_writable(report: Report, project_root: pathlib.Path) -> None:
    section = "Filesystem"
    pulci_dir = project_root / ".pulci"
    probe = pulci_dir / ".doctor_probe"
    try:
        pulci_dir.mkdir(exist_ok=True)
        probe.write_text("ok")
        probe.unlink()
    except OSError as exc:
        report.add(
            Check(
                section,
                ".pulci/ writable",
                "fail",
                f"could not write to {pulci_dir}: {exc}",
            )
        )
        return
    report.add(Check(section, ".pulci/ writable", "pass", str(pulci_dir)))


def _check_daemon_status(report: Report, project_root: pathlib.Path) -> None:
    section = "Daemon"
    pulci_dir = project_root / ".pulci"
    hb = heartbeat_info(pulci_dir)
    status = hb["daemon_status"]
    age = hb.get("heartbeat_seconds_ago")
    if status == "alive":
        report.add(
            Check(
                section,
                "running",
                "pass",
                f"heartbeat fresh ({age}s ago)",
                details={"daemon_status": status, "heartbeat_seconds_ago": age},
            )
        )
    elif status == "stale_heartbeat":
        report.add(
            Check(
                section,
                "running",
                "warn",
                f"heartbeat stale ({age}s ago) — daemon may be processing a long check",
                details={"daemon_status": status, "heartbeat_seconds_ago": age},
            )
        )
    else:
        # Dead is the common case (no daemon running). Not a failure — most
        # `pulci doctor` invocations are pre-startup checks. Surface as info.
        report.add(
            Check(
                section,
                "running",
                "pass",
                "not running (run `pulci start` to launch)",
                details={"daemon_status": status},
            )
        )


def _check_state_json(report: Report, project_root: pathlib.Path) -> None:
    section = "State"
    state_file = project_root / ".pulci" / "state.json"
    if not state_file.exists():
        # Pre-first-run case; not a failure.
        report.add(Check(section, "state.json", "pass", "not present (daemon never ran)"))
        return
    try:
        data = json.loads(state_file.read_text())
    except json.JSONDecodeError as exc:
        report.add(
            Check(
                section,
                "state.json",
                "fail",
                f"corrupted — stop pulci, remove `.pulci/state.json`, and restart. {exc}",
            )
        )
        return
    schema_version = data.get("schema_version")
    if schema_version != 1:
        report.add(
            Check(
                section,
                "state.json",
                "warn",
                f"schema_version is {schema_version!r}; this pulci expects 1",
            )
        )
        return
    report.add(
        Check(
            section,
            "state.json",
            "pass",
            f"valid (schema_version={schema_version}, state_version={data.get('state_version')})",
        )
    )


# ---------------------------------------------------------------------------
# Top-level entry
# ---------------------------------------------------------------------------


def diagnose(project_root: pathlib.Path) -> Report:
    """
    Run all checks against `project_root` and return the assembled Report.
    """
    report = Report()
    _check_project_root(report, project_root)
    if report.failed():
        # Bail early — every other check assumes a real project root.
        return report
    config = _check_pulci_toml(report, project_root)
    _check_tools(report, project_root, config)
    _check_pulci_dir_writable(report, project_root)
    _check_daemon_status(report, project_root)
    _check_state_json(report, project_root)
    return report


# ---------------------------------------------------------------------------
# Rendering
# ---------------------------------------------------------------------------


_SYMBOLS = {"pass": "✓", "warn": "!", "fail": "✗"}


def render_human(report: Report) -> str:
    """
    Pretty-print the report grouped by section. No external dependencies.
    """
    lines: list[str] = []
    sections: dict[str, list[Check]] = {}
    for c in report.checks:
        sections.setdefault(c.section, []).append(c)
    for section, checks in sections.items():
        lines.append(section)
        for c in checks:
            sym = _SYMBOLS[c.status]
            lines.append(f"  {sym} {c.name:<18} {c.message}")
        lines.append("")
    failed = report.failed()
    if failed:
        lines.append(f"{len(failed)} check(s) failed.")
    else:
        warned = [c for c in report.checks if c.status == "warn"]
        if warned:
            lines.append(f"All required checks passed ({len(warned)} warning(s) — non-blocking).")
        else:
            lines.append("All checks passed.")
    return "\n".join(lines)
