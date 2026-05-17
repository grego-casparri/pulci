"""
Tests for `pulci doctor`. The diagnostic shells out to real tools (ruff, ty,
etc.) — the test environment may or may not have them installed, so the
assertions here focus on the dispatch logic, error paths, and check
plumbing rather than asserting specific resolved versions.

The full happy-path is verified end-to-end against the live repo in the
smoke check during release.
"""

from __future__ import annotations

import json
import pathlib

from pulci.doctor import (
    Check,
    Report,
    _check_pulci_dir_writable,
    _check_pulci_toml,
    _check_state_json,
    _enabled_hooks,
    _parse_version,
    diagnose,
    render_human,
)

# --- Report mechanics ------------------------------------------------------


def test_report_failed_filters_correctly() -> None:
    r = Report()
    r.add(Check("Section", "ok", "pass", "fine"))
    r.add(Check("Section", "broken", "fail", "nope"))
    r.add(Check("Section", "watch", "warn", "meh"))
    assert len(r.failed()) == 1
    assert r.failed()[0].name == "broken"


def test_report_exit_code_zero_when_clean() -> None:
    r = Report()
    r.add(Check("X", "y", "pass", "ok"))
    r.add(Check("X", "z", "warn", "warning"))
    assert r.exit_code() == 0


def test_report_exit_code_one_on_any_failure() -> None:
    r = Report()
    r.add(Check("X", "y", "pass", "ok"))
    r.add(Check("X", "z", "fail", "broken"))
    assert r.exit_code() == 1


def test_report_to_json_includes_summary_counts() -> None:
    r = Report()
    r.add(Check("X", "a", "pass", ""))
    r.add(Check("X", "b", "pass", ""))
    r.add(Check("X", "c", "fail", ""))
    r.add(Check("X", "d", "warn", ""))
    summary = r.to_json()["summary"]
    assert summary == {"total": 4, "passed": 2, "warned": 1, "failed": 1}


# --- _parse_version --------------------------------------------------------


def test_parse_version_extracts_last_whitespace_token() -> None:
    assert _parse_version("ruff 0.7.4\n") == "0.7.4"


def test_parse_version_uses_first_line_only() -> None:
    assert _parse_version("ruff 0.7.4\nother line\n") == "0.7.4"


def test_parse_version_returns_unknown_on_empty() -> None:
    assert _parse_version("") == "unknown"
    assert _parse_version("\n") == "unknown"


# --- _enabled_hooks defaults match Rust HooksConfig::default ----------------


def test_enabled_hooks_defaults_match_rust() -> None:
    # Mirrors `impl Default for HooksConfig` in crates/pulci-core/src/config.rs.
    # If this test breaks the Rust default probably changed too; update both.
    defaults = _enabled_hooks(None)
    assert defaults == {
        "ruff": True,
        "ruff_format": False,
        "ty": True,
        "pytest": False,
        "clippy": False,
    }


def test_enabled_hooks_respects_config_overrides() -> None:
    config = {"hooks": {"ruff": False, "pytest": True}}
    enabled = _enabled_hooks(config)
    assert not enabled["ruff"]
    assert enabled["pytest"]
    # Unspecified keys stay at their defaults.
    assert enabled["ty"]


# --- _check_pulci_toml: typo detection -------------------------------------


def test_pulci_toml_typo_in_hooks_is_caught(tmp_path: pathlib.Path) -> None:
    """The Rust loader catches typos via deny_unknown_fields; doctor should
    catch them sooner with a friendlier message."""
    (tmp_path / "pulci.toml").write_text("[hooks]\nclipy = false\nruff = true\n")
    r = Report()
    _check_pulci_toml(r, tmp_path)
    fails = r.failed()
    assert len(fails) == 1
    assert "clipy" in fails[0].message


def test_pulci_toml_typo_in_tools_is_caught(tmp_path: pathlib.Path) -> None:
    (tmp_path / "pulci.toml").write_text('[tools]\nruf = "0.7.4"\n')
    r = Report()
    _check_pulci_toml(r, tmp_path)
    fails = r.failed()
    assert len(fails) == 1
    assert "ruf" in fails[0].message


def test_pulci_toml_unknown_top_level_section_is_caught(tmp_path: pathlib.Path) -> None:
    (tmp_path / "pulci.toml").write_text("[hokks]\nruff = true\n")
    r = Report()
    _check_pulci_toml(r, tmp_path)
    fails = r.failed()
    assert len(fails) == 1
    assert "hokks" in fails[0].message


def test_pulci_toml_absent_is_a_warning_not_failure(tmp_path: pathlib.Path) -> None:
    r = Report()
    _check_pulci_toml(r, tmp_path)
    # Should warn, not fail — defaults are valid behaviour.
    assert not r.failed()
    assert any(c.status == "warn" and "not present" in c.message for c in r.checks)


def test_pulci_toml_valid_passes(tmp_path: pathlib.Path) -> None:
    (tmp_path / "pulci.toml").write_text("[hooks]\nruff = true\npytest = true\n")
    r = Report()
    cfg = _check_pulci_toml(r, tmp_path)
    assert not r.failed()
    assert cfg is not None
    pass_check = next(c for c in r.checks if c.name == "pulci.toml")
    assert pass_check.status == "pass"
    assert "ruff" in pass_check.message
    assert "pytest" in pass_check.message


def test_pulci_toml_malformed_toml_is_caught(tmp_path: pathlib.Path) -> None:
    (tmp_path / "pulci.toml").write_text("[hooks\nbroken-toml\n")
    r = Report()
    _check_pulci_toml(r, tmp_path)
    fails = r.failed()
    assert len(fails) == 1
    assert "malformed" in fails[0].message.lower()


# --- Filesystem check ------------------------------------------------------


def test_pulci_dir_writable_passes_on_clean_tempdir(tmp_path: pathlib.Path) -> None:
    r = Report()
    _check_pulci_dir_writable(r, tmp_path)
    assert not r.failed()
    # And the probe file is cleaned up.
    assert not (tmp_path / ".pulci" / ".doctor_probe").exists()


# --- state.json checks -----------------------------------------------------


def test_state_json_corruption_is_caught(tmp_path: pathlib.Path) -> None:
    (tmp_path / ".pulci").mkdir()
    (tmp_path / ".pulci" / "state.json").write_text("{ not json")
    r = Report()
    _check_state_json(r, tmp_path)
    fails = r.failed()
    assert len(fails) == 1
    assert "corrupted" in fails[0].message.lower()


def test_state_json_wrong_schema_version_is_warn(tmp_path: pathlib.Path) -> None:
    (tmp_path / ".pulci").mkdir()
    (tmp_path / ".pulci" / "state.json").write_text(
        json.dumps({"schema_version": 99, "state_version": 1})
    )
    r = Report()
    _check_state_json(r, tmp_path)
    assert not r.failed()  # warn, not fail
    warn = next(c for c in r.checks if c.status == "warn")
    assert "schema_version" in warn.message


def test_state_json_absent_is_pass(tmp_path: pathlib.Path) -> None:
    r = Report()
    _check_state_json(r, tmp_path)
    assert not r.failed()


# --- diagnose end-to-end ---------------------------------------------------


def test_diagnose_returns_failure_on_missing_root() -> None:
    r = diagnose(pathlib.Path("/path/that/definitely/does/not/exist/here"))
    assert r.exit_code() == 1


def test_diagnose_returns_report_with_multiple_sections(tmp_path: pathlib.Path) -> None:
    r = diagnose(tmp_path)
    sections = {c.section for c in r.checks}
    # All major sections should appear once project root is OK.
    assert "Configuration" in sections
    assert "Filesystem" in sections
    assert "Daemon" in sections
    assert "State" in sections


# --- Renderer --------------------------------------------------------------


def test_render_human_includes_failure_summary() -> None:
    r = Report()
    r.add(Check("S", "x", "fail", "broken"))
    out = render_human(r)
    assert "✗" in out
    assert "failed" in out


def test_render_human_includes_warning_summary() -> None:
    r = Report()
    r.add(Check("S", "x", "pass", "ok"))
    r.add(Check("S", "y", "warn", "meh"))
    out = render_human(r)
    assert "!" in out
    assert "warning" in out.lower()


def test_render_human_clean_when_all_pass() -> None:
    r = Report()
    r.add(Check("S", "x", "pass", "ok"))
    out = render_human(r)
    assert "All checks passed" in out


# --- Coupling regression: known-keys lists track the Rust HooksConfig ------


def test_known_hooks_keys_covers_all_documented_fields() -> None:
    # If a new key is added to crates/pulci-core/src/config.rs::HooksConfig,
    # this test fails until doctor.py learns about it. Cheap drift guard.
    from pulci.doctor import _KNOWN_KEYS_BY_SECTION

    expected = {
        "ruff",
        "ruff_format",
        "ty",
        "pytest",
        "clippy",
        "timeout_secs",
        "pytest_test_patterns",
    }
    assert _KNOWN_KEYS_BY_SECTION["hooks"] == expected
