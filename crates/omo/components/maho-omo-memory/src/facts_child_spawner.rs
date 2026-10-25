//! In-process facts extractor child session contract.
//!
//! Mirrors upstream omo 5.1.35 ChildSpec:
//! taskId: facts-<runId>
//! cwd/sessionDir: runDir
//! toolAllowlist: [record_fact] (or [record_fact, record_agreement])
//! memberScopedTools: [record_fact tool] (and [record_agreement tool])
//! depth: 1
//! systemPrompt: facts persona
//! promptEnvelope: "bare"
//! completion: "turn"
//! prompt: "Extract durable facts from this payload and record each accepted fact with record_fact.\n\n<payload>"

use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{
        atomic::AtomicBool,
        Arc,
    },
};

use memory_core::facts::queue::FactsAbortSignal;

use crate::worker::memory_model_attempts::ReflectionModelCandidate;

#[derive(Clone)]
pub struct FactsChildSpawnInput {
    pub run_id: String,
    pub run_dir: PathBuf,
    pub model_candidates: Vec<ReflectionModelCandidate>,
    pub persona: String,
    pub prompt: String,
    pub custom_tools: Vec<maho_ext_api::ToolDefinition>,
    pub tool_allowlist: Vec<String>,
    pub hard_deadline_at: f64,
    pub signal: Option<FactsAbortSignal>,
    pub cancel_state: Arc<AtomicBool>,
}

impl std::fmt::Debug for FactsChildSpawnInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FactsChildSpawnInput")
            .field("run_id", &self.run_id)
            .field("run_dir", &self.run_dir)
            .field("model_candidates", &self.model_candidates)
            .field("persona", &self.persona)
            .field("prompt", &self.prompt)
            .field("custom_tools_count", &self.custom_tools.len())
            .field("tool_allowlist", &self.tool_allowlist)
            .field("hard_deadline_at", &self.hard_deadline_at)
            .field("signal", &self.signal)
            .field("cancel_state", &self.cancel_state)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactsChildSessionResult {
    pub completed: bool,
    pub timed_out: bool,
    pub cancelled: bool,
    pub error_message: Option<String>,
    pub attempt: u32,
    pub model: Option<String>,
}

pub type FactsChildSpawnFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FactsChildSessionResult, String>> + Send + 'a>>;

pub trait FactsChildSessionSpawner: Send + Sync {
    fn spawn<'a>(&'a self, input: FactsChildSpawnInput) -> FactsChildSpawnFuture<'a>;
}

/// Writes both outcome.json and child-exit.json to run_dir atomically, matching upstream omo
/// and facts_run_finalize expectations.
pub fn write_facts_child_artifacts(
    run_dir: &std::path::Path,
    run_id: &str,
    attempt: u32,
    result: &FactsChildSessionResult,
) -> Result<(), crate::worker::run_artifacts::ArtifactError> {
    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
    let child_exit = if result.completed {
        crate::worker::run_artifacts::ChildExit {
            code: Some(0),
            signal: None,
        }
    } else if result.timed_out {
        crate::worker::run_artifacts::ChildExit {
            code: None,
            signal: None,
        }
    } else if result.cancelled {
        crate::worker::run_artifacts::ChildExit {
            code: Some(1),
            signal: Some("SIGTERM".into()),
        }
    } else {
        crate::worker::run_artifacts::ChildExit {
            code: Some(1),
            signal: None,
        }
    };

    let outcome = crate::worker::run_artifacts::RunOutcome {
        version: 1,
        run_id: run_id.to_owned(),
        attempt: Some(attempt),
        finished_at: now.clone(),
        child_exit: child_exit.clone(),
        timed_out: result.timed_out,
        recovered_from_worktree: None,
    };
    crate::worker::run_artifacts::write_run_json_atomic(
        &run_dir.join("outcome.json"),
        &outcome,
        0o600,
    )?;

    let child_exit_artifact = crate::worker::run_artifacts::RunChildExit {
        version: 1,
        run_id: run_id.to_owned(),
        attempt,
        code: child_exit.code,
        signal: child_exit.signal,
        finished_at: now,
        timed_out: result.timed_out,
    };
    crate::worker::run_artifacts::write_run_json_atomic(
        &run_dir.join(crate::worker::run_artifacts::CHILD_EXIT_FILENAME),
        &child_exit_artifact,
        0o600,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_completed_artifacts_sets_exit_code_zero() {
        let dir = tempfile::tempdir().unwrap();
        let result = FactsChildSessionResult {
            completed: true,
            timed_out: false,
            cancelled: false,
            error_message: None,
            attempt: 1,
            model: Some("faux/faux-1".into()),
        };
        write_facts_child_artifacts(dir.path(), "run-001", 1, &result).unwrap();

        let outcome: crate::worker::run_artifacts::RunOutcome =
            crate::worker::run_artifacts::read_run_json(&dir.path().join("outcome.json")).unwrap();
        assert_eq!(outcome.child_exit.code, Some(0));
        assert!(!outcome.timed_out);
        assert_eq!(outcome.attempt, Some(1));

        let child_exit: crate::worker::run_artifacts::RunChildExit =
            crate::worker::run_artifacts::read_run_json(
                &dir.path().join(crate::worker::run_artifacts::CHILD_EXIT_FILENAME),
            )
            .unwrap();
        assert_eq!(child_exit.code, Some(0));
        assert!(!child_exit.timed_out);
    }

    #[test]
    fn write_timed_out_artifacts_sets_timed_out_flag() {
        let dir = tempfile::tempdir().unwrap();
        let result = FactsChildSessionResult {
            completed: false,
            timed_out: true,
            cancelled: false,
            error_message: Some("deadline reached".into()),
            attempt: 2,
            model: Some("faux/faux-1".into()),
        };
        write_facts_child_artifacts(dir.path(), "run-002", 2, &result).unwrap();

        let outcome: crate::worker::run_artifacts::RunOutcome =
            crate::worker::run_artifacts::read_run_json(&dir.path().join("outcome.json")).unwrap();
        assert_eq!(outcome.child_exit.code, None);
        assert!(outcome.timed_out);
        assert_eq!(outcome.attempt, Some(2));
    }

    #[test]
    fn write_cancelled_artifacts_sets_sigterm() {
        let dir = tempfile::tempdir().unwrap();
        let result = FactsChildSessionResult {
            completed: false,
            timed_out: false,
            cancelled: true,
            error_message: Some("cancelled".into()),
            attempt: 1,
            model: None,
        };
        write_facts_child_artifacts(dir.path(), "run-003", 1, &result).unwrap();

        let outcome: crate::worker::run_artifacts::RunOutcome =
            crate::worker::run_artifacts::read_run_json(&dir.path().join("outcome.json")).unwrap();
        assert_eq!(outcome.child_exit.code, Some(1));
        assert_eq!(outcome.child_exit.signal.as_deref(), Some("SIGTERM"));
        assert!(!outcome.timed_out);
    }
}

