"""Smoke tests for the Day 1 scaffold.

These exist to make CI green on the first commit and to verify that the
Rust extension is actually built and importable from the Python package.
"""

from __future__ import annotations

import pulci
from pulci import _native


def test_version_is_a_nonempty_string() -> None:
    assert isinstance(pulci.__version__, str)
    assert pulci.__version__  # non-empty


def test_native_version_matches_package_version() -> None:
    assert _native.version() == pulci.__version__
