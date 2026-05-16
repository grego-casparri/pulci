# Contributing to pulci

## Prerequisites

- **Rust** stable toolchain — install via [rustup (rustup.rs)](https://rustup.rs/)
- **uv** — Python package manager, install via `curl -LsSf https://astral.sh/uv/install.sh | sh`

## Build from source

```bash
git clone https://github.com/grego-casparri/pulci
cd pulci
uv sync
uv run maturin develop --release
uv run pulci --version   # should print 0.0.1
```

## Run the test suite

```bash
source $HOME/.cargo/env   # ensure Rust is in PATH (skip if rustup is already initialized in your shell)
uv run pytest             # Python tests
cargo test                # Rust tests
cargo clippy -- -D warnings   # must be clean
uv run ruff check .       # must be clean
uv run ruff format --check .
```

All of these must pass before a PR is merged. CI runs the same checks on
Ubuntu + macOS × Python 3.10–3.13.

## Adding a new hook adapter

pulci uses a hexagonal architecture for hook adapters: each tool gets one file
implementing the `Hook` trait.

1. Create `crates/pulci-core/src/hooks/<toolname>.rs`
2. Implement the `Hook` trait:
   - `fn name(&self) -> &'static str` — tool name used in diagnostics
   - `fn run(&self, files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>>` — invoke the tool and parse output
3. Handle `ErrorKind::NotFound` gracefully (tool not installed → return empty Vec, don't error)
4. Add `pub mod <toolname>;` to `crates/pulci-core/src/hooks/mod.rs`
5. Wire it up in `crates/pulci-py/src/lib.rs` behind a `config.hooks.<toolname>` flag
6. Add the flag to `HooksConfig` in `crates/pulci-core/src/config.rs` with an appropriate default
7. Write unit tests for the pure parser function (`parse_<toolname>_output`)

See `crates/pulci-core/src/hooks/ruff.rs` as the canonical example.

## Code style

- **Rust:** `cargo clippy -- -D warnings` must pass. No `unwrap()` outside tests.
- **Python:** `ruff check` and `ruff format` (via `uv run ruff ...`).
- **Commits:** [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`, `chore:`).
- **No emojis** in code or docs.

## Pull requests

- Keep PRs focused. One logical change per PR.
- Add or update tests for every code change.
- Update `CHANGELOG.md` under `[Unreleased]`.
- CI must be green before merging.

## License

By contributing, you agree that your contributions will be licensed under
Apache-2.0, the same license as this project.
