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
/// Re-exported by `pulci-py` for Python so `pulci --version` has a
/// canonical source of truth.
pub fn version() -> &'static str {
    VERSION
}

pub mod cache;
pub mod config;
pub mod event_trace;
pub mod hooks;
pub mod orchestrator;
pub mod resolver;
pub mod scan;
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
