use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub mod cargo;
pub mod pytest;
pub mod ruff;
pub mod ty;

/// Normalized severity level shared across all hook adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl std::fmt::Display for Severity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Severity::Error => write!(f, "error"),
            Severity::Warning => write!(f, "warning"),
            Severity::Info => write!(f, "info"),
        }
    }
}

/// A single diagnostic emitted by a quality-gate tool.
///
/// This is the unified type that every hook adapter must produce.
/// Day 4 will aggregate these into `.pulci/state.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostic {
    pub tool: String,
    pub file: PathBuf,
    pub line: u32,
    pub col: u32,
    pub severity: Severity,
    pub code: Option<String>,
    pub message: String,
}

/// A quality-gate tool adapter.
///
/// Each adapter shells out to its canonical tool and normalises the output
/// into `Vec<Diagnostic>`. Adding a new tool means implementing this trait
/// in a new file — nothing else changes.
pub trait Hook: Send + Sync {
    fn name(&self) -> &'static str;
    fn run(&self, files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>>;
}
