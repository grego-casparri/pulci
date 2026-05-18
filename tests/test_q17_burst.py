"""
Repro determinista para Q-17 (event loss bajo ráfaga).

Escribe N archivos .py en burst, espera settling, valida que los N aparecen
en state.json. Si falla: parsea events.log y reporta dónde se perdió cada
archivo cross-stage (watcher → debounce → cache → state_write).

Marcado `xfail` hasta que el fix de Q-17 aterrice; en ese momento vira a
passing y queda como regression guard. `strict=False` para que un xpass
(bug no reproducido en este hardware/run) no rompa CI.
"""

from __future__ import annotations

import json
import pathlib
import subprocess
import sys
import time

import pytest

PULCI_BIN = str(pathlib.Path(sys.executable).parent / "pulci")


def _wait_for_state(state_file: pathlib.Path, timeout_s: float = 15.0) -> dict | None:
    """
    Block until state.json exists and parses. Returns the parsed dict or None.
    """
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        if state_file.exists():
            try:
                return json.loads(state_file.read_text())
            except json.JSONDecodeError:
                pass
        time.sleep(0.05)
    return None


def _diagnose_missing(missing: set[str], events_log: pathlib.Path) -> str:
    """
    Parse events.log and summarize the journey of each missing file.
    """
    if not events_log.exists():
        return f"{len(missing)} archivos faltantes y events.log no existe"

    events: list[dict] = []
    for line in events_log.read_text().splitlines():
        if not line.strip():
            continue
        try:
            events.append(json.loads(line))
        except json.JSONDecodeError:
            continue

    lines = [f"{len(missing)} archivos faltantes en state.json:"]
    for m in sorted(missing):
        watcher_hits = sum(1 for e in events if e.get("stage") == "watcher" and e.get("path") == m)
        in_debounce = sum(
            1
            for e in events
            if e.get("stage") == "debounce"
            and e.get("action") == "window_close"
            and m in (e.get("files") or [])
        )
        cache_decisions = [
            e.get("decision") for e in events if e.get("stage") == "cache" and e.get("path") == m
        ]
        lines.append(
            f"  {m}: watcher={watcher_hits}, debounce_batches={in_debounce}, "
            f"cache_decisions={cache_decisions}"
        )
    return "\n".join(lines)


@pytest.mark.xfail(
    reason="Q-17: se espera fix antes de virar a passing; strict=False permite xpass",
    strict=False,
)
def test_burst_of_8_files_all_reflected_in_state(tmp_path: pathlib.Path) -> None:
    (tmp_path / "pulci.toml").write_text(
        "[hooks]\nruff = true\nty = false\n\n[debug]\nevent_trace = true\n"
    )

    # Sembrar baseline para que el scan inicial tenga algo limpio que procesar.
    (tmp_path / "baseline.py").write_text("# baseline\n")

    proc = subprocess.Popen(
        [PULCI_BIN, "start", str(tmp_path), "--agent"],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )
    state_file = tmp_path / ".pulci" / "state.json"
    try:
        first = _wait_for_state(state_file, timeout_s=15.0)
        assert first is not None, "state.json never appeared during initial scan"
        baseline_version = first.get("state_version", 0)

        # BURST: 8 archivos con violación ruff F401 (unused import) determinística.
        # `unused_X = 1` a nivel módulo NO dispara ruff (top-level assignments
        # son válidos); `import os` sin uso sí, en todas las versiones de ruff.
        for i in range(8):
            (tmp_path / f"burst_{i}.py").write_text(f"import os  # burst_{i}\n")

        # Settling: esperar a que state_version avance varias veces y
        # estabilice. 5s holgura para hardware lento de CI.
        time.sleep(5.0)

        final = json.loads(state_file.read_text())
        assert final["state_version"] > baseline_version, (
            "state_version did not advance after burst"
        )

        diag_files = {d["file"] for d in final.get("diagnostics", [])}
        expected = {str(tmp_path / f"burst_{i}.py") for i in range(8)}
        missing = expected - diag_files

        if missing:
            pytest.fail(_diagnose_missing(missing, tmp_path / ".pulci" / "events.log"))
    finally:
        proc.terminate()
        try:
            proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait(timeout=2)
