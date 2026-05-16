"""pulci: continuous quality gates for agent-driven Python development."""

from pulci._native import version as _native_version

__version__: str = _native_version()

__all__ = ["__version__"]
