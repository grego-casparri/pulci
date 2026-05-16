//! pulci-core
//!
//! The engine that powers pulci. Pure Rust, no Python dependency.
//! Day 1 ships a version sentinel; subsequent days add the watcher,
//! the hook orchestrator, and the state aggregator.

#![warn(clippy::all)]

/// The crate version, read from Cargo at compile time.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Returns the pulci-core version as a static string.
///
/// This is the function exposed through PyO3 to Python on Day 1
/// so that `pulci --version` has something real to print while the
/// daemon machinery is being built out.
pub fn version() -> &'static str {
    VERSION
}

pub mod cache;
pub mod config;
pub mod hooks;
pub mod orchestrator;
pub mod resolver;
pub mod state;
pub mod watcher;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_not_empty() {
        assert!(!version().is_empty());
    }
}
