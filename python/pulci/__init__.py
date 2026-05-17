"""
pulci: continuous quality gates for agent-driven Python development.
"""

try:
    from pulci._native import version as _native_version
except ImportError as _e:
    raise ImportError(
        "pulci._native extension not found. "
        "The Rust extension may not have been built for this platform. "
        "Install from source: pip install git+https://github.com/grego-casparri/pulci\n"
        f"Original error: {_e}"
    ) from _e

__version__: str = _native_version()

__all__ = ["__version__"]
