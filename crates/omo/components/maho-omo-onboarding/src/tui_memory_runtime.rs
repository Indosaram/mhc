//! Bodies of the TUI's "understanding you" tools (upstream `tui-memory-runtime.ts`): `onboarding_understand`
//! runs the same understanding pass the RPC runs and holds its candidates in memory; `onboarding_memory_apply`
//! writes only the selected ones through the session facts applier (one commit).

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, PoisonError},
};

use maho_ai::utils::abort::AbortSignal;
use maho_ext_api::{AgentToolResult, ExtensionContext, ModelRegistry};
use memory_core::facts::extraction::FactsExtractionRecord;
use serde_json::{Value, json};

use crate::tui_scan_runtime::TuiScanResult;
use crate::understand_contract::{
    OnboardingFactCandidate, OnboardingUnderstandEventKind, UnderstandFallback,
};
use crate::understand_start::{
    OnboardingUnderstandingSeams, OnboardingUnderstandingStart, OnboardingUnderstandingStartResult,
    start_onboarding_understanding,
};

/// Candidates of open understandings, by understand id; in memory only.
pub type Runs = Arc<Mutex<HashMap<String, Vec<OnboardingFactCandidate>>>>;

/// The TUI's scan, run once per process and kept only on success.
pub type TuiScan = Arc<dyn Fn() -> Option<Arc<TuiScanResult>> + Send + Sync>;

/// How the session's facts applier answers (`omo.memory.facts.apply`'s response).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryFactsApplyResponse {
    Committed {
        sha: String,
        affected_paths: Vec<String>,
    },
    /// Any other terminal outcome, e.g. `no_facts`.
    Outcome {
        outcome: String,
        affected_paths: Vec<String>,
    },
    /// The write was refused or failed; nothing was written.
    Failed { reason: String },
}

/// The session facts applier: writes the records in one memory commit, idempotent per batch.
pub trait MemoryFactsApplier: Send + Sync {
    fn apply(
        &self,
        records: Vec<FactsExtractionRecord>,
        session_id: Option<String>,
    ) -> MemoryFactsApplyResponse;
}

pub struct UnderstandInput {
    pub consent_model: Option<String>,
    pub signal: Option<AbortSignal>,
    pub runs: Runs,
    pub scan: TuiScan,
    pub seams: Option<OnboardingUnderstandingSeams>,
}

fn failure(text: impl Into<String>, details: Value) -> AgentToolResult {
    let mut result = AgentToolResult::text(text);
    result.details = details;
    result.is_error = Some(true);
    result
}

fn current_model(ctx: &ExtensionContext) -> Option<String> {
    ctx.model
        .as_ref()
        .map(|model| format!("{}/{}", model.provider, model.id))
}

pub fn format_candidates(
    understand_id: &str,
    candidates: &[OnboardingFactCandidate],
    fallback: bool,
    steps: &[String],
) -> String {
    let mut lines = vec![format!("understanding {understand_id}")];
    lines.extend(steps.iter().map(|step| format!("step: {step}")));
    lines.push(if fallback {
        "The model could not read the sessions; continue from the scan rows only.".to_owned()
    } else {
        format!(
            "{} candidate {} (nothing is saved yet):",
            candidates.len(),
            if candidates.len() == 1 {
                "fact"
            } else {
                "facts"
            }
        )
    });
    for candidate in candidates {
        let who = match &candidate.fact {
            FactsExtractionRecord::Person { person, .. } => format!("about {}", person.name),
            FactsExtractionRecord::Project { .. } => "project".to_owned(),
        };
        lines.push(format!(
            "- [{}] ({who}, from {} sessions) {}",
            candidate.id,
            candidate.sources.len(),
            candidate.fact.text()
        ));
    }
    lines.join("\n")
}

pub async fn execute_onboarding_understand(
    ctx: &ExtensionContext,
    input: UnderstandInput,
) -> AgentToolResult {
    let Some(model) = input.consent_model.or_else(|| current_model(ctx)) else {
        return failure(
            "No model is selected in this session; pass model as provider/model.",
            Value::Null,
        );
    };
    let registry: Arc<dyn ModelRegistry> = Arc::clone(&ctx.model_registry);
    let scan = Arc::clone(&input.scan);
    let scan = match tokio::task::spawn_blocking(move || scan()).await {
        Ok(Some(scan)) => scan,
        _ => {
            return failure(
                "The onboarding scan did not run; continue with the scan rows only.",
                Value::Null,
            );
        }
    };
    let Some(seams) = input.seams else {
        return failure(
            "The onboarding understanding launcher is not wired in this build.",
            Value::Null,
        );
    };
    let steps = Arc::new(Mutex::new(Vec::<String>::new()));
    let step_sink = Arc::clone(&steps);
    let started = start_onboarding_understanding(
        OnboardingUnderstandingStart {
            scan_id: scan.scan_id.clone(),
            model,
            digests: scan.digests.clone(),
            project_roots: scan.project_roots.clone(),
            model_registry: registry,
            env: scan.env.clone(),
            seams,
        },
        Arc::new(move |event| {
            if event.kind == OnboardingUnderstandEventKind::StepDone
                && let Some(step) = event.step
            {
                step_sink
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .push(step.label);
            }
        }),
    );
    let started = match started {
        OnboardingUnderstandingStartResult::Unavailable(reply) => {
            return failure(reply.reason, Value::Null);
        }
        OnboardingUnderstandingStartResult::Started(started) => started,
    };
    let understand_id = started.understand_id.clone();
    let control = Arc::clone(&started.control);
    // An abort of the tool call stops the run; it still ends with the candidates it already has.
    let listener = input.signal.as_ref().map(|signal| {
        let stop = Arc::clone(&control);
        (
            signal.clone(),
            signal.add_abort_listener(move |_| stop.stop()),
        )
    });
    let done = tokio::task::spawn_blocking(move || started.finished.join()).await;
    if let Some((signal, id)) = listener {
        signal.remove_abort_listener(id);
    }
    let Ok(Ok(done)) = done else {
        return failure("The understanding run ended unexpectedly.", Value::Null);
    };
    let candidates = done.candidates.unwrap_or_default();
    let fallback = done.fallback == Some(UnderstandFallback::ScanOnly);
    input
        .runs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(understand_id.clone(), candidates.clone());
    let steps = steps.lock().unwrap_or_else(PoisonError::into_inner).clone();
    let mut details = json!({"understandId": understand_id, "candidates": candidates});
    if fallback {
        details["fallback"] = json!("scan_only");
    }
    let mut result = AgentToolResult::text(format_candidates(
        &understand_id,
        &candidates,
        fallback,
        &steps,
    ));
    result.details = details;
    result
}

pub struct MemoryApplyInput {
    pub understand_id: String,
    pub selected: Vec<String>,
    pub session_id: Option<String>,
    pub runs: Runs,
    pub applier: Option<Arc<dyn MemoryFactsApplier>>,
}

pub async fn execute_onboarding_memory_apply(input: MemoryApplyInput) -> AgentToolResult {
    let candidates = input
        .runs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(&input.understand_id)
        .cloned();
    let Some(candidates) = candidates else {
        return failure(
            "No open understanding with this id; its facts were already remembered or it never ran.",
            Value::Null,
        );
    };
    let unknown: Vec<&str> = input
        .selected
        .iter()
        .filter(|id| !candidates.iter().any(|c| &c.id == *id))
        .map(String::as_str)
        .collect();
    if !unknown.is_empty() {
        return failure(
            format!(
                "Not candidates of this understanding: {}. Nothing was written.",
                unknown.join(", ")
            ),
            Value::Null,
        );
    }
    let records: Vec<FactsExtractionRecord> = candidates
        .iter()
        .filter(|c| input.selected.contains(&c.id))
        .map(|c| c.fact.clone())
        .collect();
    if records.is_empty() {
        let mut result = AgentToolResult::text("Nothing selected; nothing was written.");
        result.details = json!({"outcome": "no_facts", "affectedPaths": []});
        return result;
    }
    let Some(applier) = input.applier else {
        return failure(
            "Memory is off in this session, so nothing was written.",
            Value::Null,
        );
    };
    let count = records.len();
    let session_id = input.session_id;
    let response =
        match tokio::task::spawn_blocking(move || applier.apply(records, session_id)).await {
            Ok(response) => response,
            Err(error) => MemoryFactsApplyResponse::Failed {
                reason: error.to_string(),
            },
        };
    match &response {
        MemoryFactsApplyResponse::Committed {
            sha,
            affected_paths,
        } => {
            input
                .runs
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .remove(&input.understand_id);
            let mut result = AgentToolResult::text(format!(
                "Remembered {count} {} in one memory commit {}: {}",
                if count == 1 { "fact" } else { "facts" },
                sha.chars().take(12).collect::<String>(),
                affected_paths.join(", ")
            ));
            result.details =
                json!({"outcome": "committed", "sha": sha, "affectedPaths": affected_paths});
            result
        }
        MemoryFactsApplyResponse::Outcome {
            outcome,
            affected_paths,
        } => {
            let mut result = AgentToolResult::text(format!("Not written ({outcome})."));
            result.details = json!({"outcome": outcome, "affectedPaths": affected_paths});
            result
        }
        MemoryFactsApplyResponse::Failed { reason } => {
            let mut result = AgentToolResult::text(format!("Not written: {reason}"));
            result.details = json!({"kind": "unavailable", "reason": reason});
            result.is_error = Some(true);
            result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_ai::types::ContentBlock;
    use memory_core::facts::extraction::FactsPersonReference;

    fn text_of(result: &AgentToolResult) -> String {
        result
            .content
            .iter()
            .filter_map(|block| match block {
                ContentBlock::Text(text) => Some(text.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn candidate(id: &str, fact: FactsExtractionRecord) -> OnboardingFactCandidate {
        OnboardingFactCandidate {
            id: id.into(),
            fact,
            sources: vec!["a".into(), "b".into()],
        }
    }

    fn project(text: &str) -> FactsExtractionRecord {
        FactsExtractionRecord::Project {
            receipt: None,
            text: text.into(),
            date: "2026-10-22".into(),
        }
    }

    struct Applier(MemoryFactsApplyResponse);
    impl MemoryFactsApplier for Applier {
        fn apply(
            &self,
            records: Vec<FactsExtractionRecord>,
            _: Option<String>,
        ) -> MemoryFactsApplyResponse {
            assert_eq!(records.len(), 1);
            self.0.clone()
        }
    }

    fn runs_with(id: &str, candidates: Vec<OnboardingFactCandidate>) -> Runs {
        let runs: Runs = Arc::default();
        runs.lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id.into(), candidates);
        runs
    }

    fn input(
        runs: &Runs,
        selected: &[&str],
        applier: Option<Arc<dyn MemoryFactsApplier>>,
    ) -> MemoryApplyInput {
        MemoryApplyInput {
            understand_id: "u".into(),
            selected: selected.iter().map(|s| (*s).to_owned()).collect(),
            session_id: None,
            runs: Arc::clone(runs),
            applier,
        }
    }

    #[test]
    fn candidates_are_listed_with_their_subject_and_session_count() {
        let person = FactsExtractionRecord::Person {
            receipt: None,
            person: FactsPersonReference {
                name: "Ada".into(),
                aliases: vec![],
            },
            text: "likes tests".into(),
            date: "2026-10-22".into(),
        };
        let text = format_candidates(
            "u",
            &[
                candidate("fact-1", project("uses rust")),
                candidate("fact-2", person),
            ],
            false,
            &["Read 2 recent sessions in p".into()],
        );
        assert_eq!(
            text,
            "understanding u\nstep: Read 2 recent sessions in p\n2 candidate facts (nothing is saved yet):\n- [fact-1] (project, from 2 sessions) uses rust\n- [fact-2] (about Ada, from 2 sessions) likes tests"
        );
        assert!(
            format_candidates("u", &[], true, &[]).contains(
                "The model could not read the sessions; continue from the scan rows only."
            )
        );
    }

    #[tokio::test]
    async fn apply_refuses_unknown_ids_and_unopened_runs_before_writing() {
        let runs = runs_with("u", vec![candidate("fact-1", project("x"))]);
        let none = execute_onboarding_memory_apply(MemoryApplyInput {
            understand_id: "zzz".into(),
            ..input(&runs, &[], None)
        })
        .await;
        assert_eq!(
            text_of(&none),
            "No open understanding with this id; its facts were already remembered or it never ran."
        );
        let unknown = execute_onboarding_memory_apply(input(&runs, &["fact-9"], None)).await;
        assert_eq!(
            text_of(&unknown),
            "Not candidates of this understanding: fact-9. Nothing was written."
        );
        let empty = execute_onboarding_memory_apply(input(&runs, &[], None)).await;
        assert_eq!(text_of(&empty), "Nothing selected; nothing was written.");
        assert_eq!(empty.details["outcome"], "no_facts");
        let off = execute_onboarding_memory_apply(input(&runs, &["fact-1"], None)).await;
        assert_eq!(
            (text_of(&off).as_str(), off.is_error),
            (
                "Memory is off in this session, so nothing was written.",
                Some(true)
            )
        );
    }

    #[tokio::test]
    async fn a_committed_apply_reports_the_commit_and_closes_the_run() {
        let runs = runs_with(
            "u",
            vec![
                candidate("fact-1", project("x")),
                candidate("fact-2", project("y")),
            ],
        );
        let applier: Arc<dyn MemoryFactsApplier> =
            Arc::new(Applier(MemoryFactsApplyResponse::Committed {
                sha: "0123456789abcdef".into(),
                affected_paths: vec!["a.md".into()],
            }));
        let done = execute_onboarding_memory_apply(input(&runs, &["fact-2"], Some(applier))).await;
        assert_eq!(
            text_of(&done),
            "Remembered 1 fact in one memory commit 0123456789ab: a.md"
        );
        assert!(
            runs.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_empty()
        );
    }
}
