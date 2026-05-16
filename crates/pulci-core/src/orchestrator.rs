use std::path::PathBuf;
use std::sync::Arc;

use crate::hooks::{Diagnostic, Hook};

/// Result of running a single hook.
#[derive(Debug)]
pub struct RunResult {
    pub tool: String,
    pub diagnostics: Vec<Diagnostic>,
    /// Non-None if the hook failed to execute (not the same as finding violations).
    pub error: Option<String>,
}

/// Runs a collection of hooks in parallel using `tokio::task::spawn_blocking`.
///
/// Each hook blocks a thread from the blocking thread pool while its subprocess
/// runs. Results are collected once all hooks finish.
pub struct Orchestrator {
    hooks: Vec<Arc<dyn Hook>>,
}

impl Orchestrator {
    pub fn new(hooks: Vec<Arc<dyn Hook>>) -> Self {
        Self { hooks }
    }

    pub async fn run(&self, files: &[PathBuf]) -> Vec<RunResult> {
        let mut set = tokio::task::JoinSet::new();

        for hook in &self.hooks {
            let hook = Arc::clone(hook);
            let files = files.to_vec();
            set.spawn_blocking(move || {
                let tool = hook.name().to_owned();
                match hook.run(&files) {
                    Ok(diagnostics) => RunResult {
                        tool,
                        diagnostics,
                        error: None,
                    },
                    Err(e) => RunResult {
                        tool,
                        diagnostics: vec![],
                        error: Some(e.to_string()),
                    },
                }
            });
        }

        let mut results = Vec::new();
        while let Some(outcome) = set.join_next().await {
            match outcome {
                Ok(r) => results.push(r),
                Err(e) => results.push(RunResult {
                    tool: "unknown".into(),
                    diagnostics: vec![],
                    error: Some(format!("hook task panicked: {e}")),
                }),
            }
        }
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hooks::Severity;

    struct AlwaysOkHook {
        name: &'static str,
        diag: Diagnostic,
    }

    impl Hook for AlwaysOkHook {
        fn name(&self) -> &'static str {
            self.name
        }
        fn run(&self, _files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>> {
            Ok(vec![self.diag.clone()])
        }
    }

    struct AlwaysErrHook;

    impl Hook for AlwaysErrHook {
        fn name(&self) -> &'static str {
            "broken"
        }
        fn run(&self, _files: &[PathBuf]) -> anyhow::Result<Vec<Diagnostic>> {
            anyhow::bail!("tool not found")
        }
    }

    fn make_diag(tool: &str) -> Diagnostic {
        Diagnostic {
            tool: tool.into(),
            file: PathBuf::from("x.py"),
            line: 1,
            col: 0,
            severity: Severity::Error,
            code: None,
            message: "test".into(),
        }
    }

    #[tokio::test]
    async fn collects_results_from_multiple_hooks() {
        let hooks: Vec<Arc<dyn Hook>> = vec![
            Arc::new(AlwaysOkHook {
                name: "hook-a",
                diag: make_diag("hook-a"),
            }),
            Arc::new(AlwaysOkHook {
                name: "hook-b",
                diag: make_diag("hook-b"),
            }),
        ];
        let orch = Orchestrator::new(hooks);
        let results = orch.run(&[]).await;
        assert_eq!(results.len(), 2);
        assert!(results.iter().all(|r| r.error.is_none()));
        assert!(results.iter().all(|r| r.diagnostics.len() == 1));
    }

    #[tokio::test]
    async fn error_hook_does_not_panic_orchestrator() {
        let hooks: Vec<Arc<dyn Hook>> = vec![Arc::new(AlwaysErrHook)];
        let orch = Orchestrator::new(hooks);
        let results = orch.run(&[]).await;
        assert_eq!(results.len(), 1);
        assert!(results[0].error.is_some());
        assert!(results[0].diagnostics.is_empty());
    }

    #[tokio::test]
    async fn empty_hooks_returns_empty() {
        let orch = Orchestrator::new(vec![]);
        let results = orch.run(&[]).await;
        assert!(results.is_empty());
    }
}
