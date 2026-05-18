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
uv run pulci --version   # should print 0.0.6
```

## Commit hooks (prek)

pulci uses [prek](https://github.com/astral-sh/prek) for local commit-time quality gates.

Install prek once on your machine:

```bash
cargo install prek
```

Then activate the hooks in your clone:

```bash
prek install
```

This installs two hooks:
- **pre-commit**: ruff check + ruff format + cargo clippy (~5 s)
- **pre-push**: pytest + cargo test (~30 s)

prek is optional on Windows (support is still maturing upstream). Run the checks manually instead:

```bash
uv run ruff check . && uv run ruff format --check . && cargo clippy --workspace -- -D warnings
uv run pytest && cargo test --workspace
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
6. Add the flag to `HooksConfig` in `crates/pulci-core/src/config.rs`. Default to `true` only if the tool is universally present in Python projects (like ruff/ty); default to `false` if it requires extra setup or is language-specific (like pytest, clippy)
7. Write unit tests for the pure parser function (`parse_<toolname>_output`)

See `crates/pulci-core/src/hooks/ruff.rs` as the canonical example.

## Code style

- **Rust:** `cargo clippy -- -D warnings` must pass. No `unwrap()` outside tests.
- **Python:** `ruff check` and `ruff format` (via `uv run ruff ...`).
- **Commits:** [Conventional Commits](https://www.conventionalcommits.org/) (`feat:`, `fix:`, `docs:`, `chore:`).
- **No emojis** in code or docs.

## Regression tests

**Every bug fix lands with a regression test.** The test must:

1. Reproduce the bug on the codebase *before* the fix is applied (fail).
2. Pass after the fix is applied (green).
3. Live in the smallest scope that captures the failure — prefer Rust unit
   tests for adapter or parser bugs, Python integration tests for daemon
   lifecycle, watcher, CLI, or MCP surface bugs.

The only exemptions are pure documentation changes and configuration changes
that have no observable runtime behavior. Everything else — including tiny
one-line fixes — needs a test that would have caught the bug.

PR authors include the test in the same commit as the fix. Reviewers reject
PRs that fix a bug without a regression test and ask for the test to be
added before merging. When in doubt, write the test first, revert the fix
locally, confirm the test fails, restore the fix.

## Pull requests

- Keep PRs focused. One logical change per PR.
- Add or update tests for every code change.
- Bug fixes follow the regression-test policy above.
- Update `CHANGELOG.md` under `[Unreleased]`.
- CI must be green before merging.

## License

By contributing, you agree that your contributions will be licensed under
Apache-2.0, the same license as this project.
