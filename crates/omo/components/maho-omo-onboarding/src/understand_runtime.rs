//! "Understanding you" (upstream `understand-runtime.ts`): the facts extractor reads the scan's session
//! digests (plus each project's AGENTS.md) one project at a time and proposes facts. Candidates only;
//! nothing reaches memory here.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{
        Arc,
        atomic::{AtomicU32, Ordering},
    },
    time::Instant,
};

use memory_core::facts::extraction::FactsExtractionRecord;

use crate::session_history::SessionDigest;
use crate::understand_contract::{
    ONBOARDING_UNDERSTAND_BUDGET, OnboardingFactCandidate, OnboardingUnderstandBudgetKind as Kind,
    OnboardingUnderstandBudgetLimits, OnboardingUnderstandEvent,
    OnboardingUnderstandEventKind as EventKind, OnboardingUnderstandStep, UnderstandFallback,
};
use crate::understand_control::OnboardingUnderstandingControl;
use crate::understand_step::{
    RunStepInput, StepRead, StepResult, UnderstandingModel, read_step, run_step, step_payload,
};

pub struct OnboardingUnderstandingInput {
    pub understand_id: String,
    pub scan_id: String,
    pub digests: Vec<SessionDigest>,
    /// Project roots the scan listed; a step reads AGENTS.md only inside one of these.
    pub project_roots: HashSet<String>,
    pub model: UnderstandingModel,
    pub control: Arc<OnboardingUnderstandingControl>,
    pub budget: Option<OnboardingUnderstandBudgetLimits>,
    pub env: Option<BTreeMap<String, String>>,
    pub today: Option<String>,
    pub now: Option<Arc<dyn Fn() -> f64 + Send + Sync>>,
}

struct StepGroup {
    project: Option<String>,
    digests: Vec<SessionDigest>,
}

struct Candidate {
    key: String,
    id: String,
    fact: FactsExtractionRecord,
    sources: Vec<String>,
}

impl Candidate {
    fn public(&self) -> OnboardingFactCandidate {
        OnboardingFactCandidate {
            id: self.id.clone(),
            fact: self.fact.clone(),
            sources: self.sources.clone(),
        }
    }
}

#[derive(Default)]
struct EventBody {
    kind: Option<EventKind>,
    step: Option<OnboardingUnderstandStep>,
    fact: Option<OnboardingFactCandidate>,
    budget: Option<Kind>,
    reason: Option<String>,
    retrying: bool,
    skipped: bool,
    candidates: Option<Vec<OnboardingFactCandidate>>,
    fallback: bool,
    stopped: bool,
}

fn basename(path: &str) -> String {
    std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.to_owned(), |n| n.to_string_lossy().into_owned())
}

fn fact_key(record: &FactsExtractionRecord) -> String {
    let text = record
        .text()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    match record {
        FactsExtractionRecord::Person { person, .. } => {
            format!("person:{}:{text}", person.name.to_lowercase())
        }
        FactsExtractionRecord::Project { .. } => format!("project:{text}"),
    }
}

fn plan_steps(digests: &[SessionDigest], min_sessions: usize) -> Vec<StepGroup> {
    let mut newest_first = digests.to_vec();
    newest_first.sort_by(|left, right| right.last_active_at.cmp(&left.last_active_at));
    let mut order: Vec<String> = Vec::new();
    let mut by_project: BTreeMap<String, Vec<SessionDigest>> = BTreeMap::new();
    for digest in newest_first {
        if !by_project.contains_key(&digest.cwd) {
            order.push(digest.cwd.clone());
        }
        by_project
            .entry(digest.cwd.clone())
            .or_default()
            .push(digest);
    }
    let mut groups = Vec::new();
    let mut singles = Vec::new();
    for project in order {
        let own = by_project.remove(&project).unwrap_or_default();
        if own.len() >= min_sessions {
            groups.push(StepGroup {
                project: Some(project),
                digests: own,
            });
        } else {
            singles.extend(own);
        }
    }
    if singles.len() >= min_sessions {
        groups.push(StepGroup {
            project: None,
            digests: singles,
        });
    }
    groups
}

fn describe_step(id: String, project: Option<&str>, read: &StepRead) -> OnboardingUnderstandStep {
    let sessions = read.digests.len();
    match project {
        Some(project) => OnboardingUnderstandStep {
            id,
            label: format!("Read {sessions} recent sessions in {}", basename(project)),
            project: Some(project.to_owned()),
            sessions,
        },
        None => {
            let projects = read
                .digests
                .iter()
                .map(|d| d.cwd.as_str())
                .collect::<HashSet<_>>()
                .len();
            OnboardingUnderstandStep {
                id,
                label: format!("Read {sessions} recent sessions across {projects} projects"),
                project: None,
                sessions,
            }
        }
    }
}

/// Runs the whole understanding pass and returns the final `done` event. Scratch directories are removed
/// before this returns.
pub fn run_onboarding_understanding(
    input: OnboardingUnderstandingInput,
    emit: &mut dyn FnMut(OnboardingUnderstandEvent),
) -> OnboardingUnderstandEvent {
    let budget = input.budget.unwrap_or(ONBOARDING_UNDERSTAND_BUDGET);
    let today = input.today.clone().unwrap_or_else(|| {
        jiff::Timestamp::now()
            .to_string()
            .chars()
            .take(10)
            .collect()
    });
    let mut model = input.model.clone();
    if let Some(env) = &input.env {
        model.env = env.clone();
    }
    let control = Arc::clone(&input.control);
    let started = Instant::now();
    let now: Arc<dyn Fn() -> f64 + Send + Sync> = input
        .now
        .clone()
        .unwrap_or_else(move || Arc::new(move || started.elapsed().as_secs_f64() * 1000.0));
    let started_at = now();
    let elapsed = || (now() - started_at).round().max(0.0) as u64;
    let mut bytes_read = 0_usize;
    let tool_calls = Arc::new(AtomicU32::new(0));
    let mut candidates: Vec<Candidate> = Vec::new();

    let mut send = |body: EventBody,
                    bytes_read: usize,
                    tool_calls: u32,
                    elapsed_ms: u64|
     -> OnboardingUnderstandEvent {
        let event = OnboardingUnderstandEvent {
            understand_id: input.understand_id.clone(),
            scan_id: input.scan_id.clone(),
            kind: body.kind.unwrap_or(EventKind::Done),
            step: body.step,
            fact: body.fact,
            budget: body.budget,
            bytes_read,
            tool_calls,
            elapsed_ms,
            reason: body.reason,
            retrying: body.retrying.then_some(true),
            skipped: body.skipped.then_some(true),
            candidates: body.candidates,
            fallback: body.fallback.then_some(UnderstandFallback::ScanOnly),
            stopped: body.stopped.then_some(true),
        };
        emit(event.clone());
        event
    };
    macro_rules! emit_body {
        ($body:expr) => {
            send(
                $body,
                bytes_read,
                tool_calls.load(Ordering::SeqCst),
                elapsed(),
            )
        };
    }
    macro_rules! finish {
        ($extra:expr) => {{
            let body = EventBody {
                kind: Some(EventKind::Done),
                candidates: Some(candidates.iter().map(Candidate::public).collect()),
                ..$extra
            };
            return emit_body!(body);
        }};
    }
    let kind_of = |kind: EventKind| EventBody {
        kind: Some(kind),
        ..EventBody::default()
    };

    let mut step_number = 0_usize;
    for group in plan_steps(&input.digests, budget.min_source_sessions) {
        if control.stopped() {
            finish!(EventBody {
                stopped: true,
                ..EventBody::default()
            });
        }
        let live: Vec<SessionDigest> = group
            .digests
            .iter()
            .filter(|d| !control.is_skipped(&d.cwd))
            .cloned()
            .collect();
        if live.len() < budget.min_source_sessions {
            continue;
        }
        if budget.total_ms as i64 - elapsed() as i64 <= 0 {
            emit_body!(EventBody {
                budget: Some(Kind::TotalMs),
                ..kind_of(EventKind::Budget)
            });
            finish!(EventBody::default());
        }
        let read = read_step(
            &live,
            group.project.as_deref(),
            budget.read_bytes.saturating_sub(bytes_read),
            &input.project_roots,
        );
        if read.digests.len() < budget.min_source_sessions {
            emit_body!(EventBody {
                budget: Some(Kind::ReadBytes),
                ..kind_of(EventKind::Budget)
            });
            finish!(EventBody::default());
        }
        bytes_read += read.bytes;
        step_number += 1;
        let step = describe_step(
            format!("step-{step_number}"),
            group.project.as_deref(),
            &read,
        );
        emit_body!(EventBody {
            step: Some(step.clone()),
            ..kind_of(EventKind::StepStarted)
        });

        let projects: HashSet<String> = read.digests.iter().map(|d| d.cwd.clone()).collect();
        let run_once = || {
            let counted = Arc::clone(&tool_calls);
            run_step(RunStepInput {
                model: &model,
                payload: step_payload(
                    &read,
                    group.project.as_deref(),
                    &today,
                    budget.min_source_sessions,
                ),
                deadline_ms: budget
                    .step_ms
                    .min(budget.total_ms.saturating_sub(elapsed())),
                max_calls: budget
                    .tool_calls
                    .saturating_sub(tool_calls.load(Ordering::SeqCst)),
                on_call: Arc::new(move || {
                    counted.fetch_add(1, Ordering::SeqCst);
                }),
                control: &control,
                projects: projects.clone(),
            })
        };
        let mut result = run_once();
        if let StepResult::Failed { reason, .. } = &result {
            if budget.total_ms as i64 - elapsed() as i64 <= 0 {
                emit_body!(EventBody {
                    step: Some(step.clone()),
                    reason: Some(reason.clone()),
                    ..kind_of(EventKind::StepFailed)
                });
                emit_body!(EventBody {
                    budget: Some(Kind::TotalMs),
                    ..kind_of(EventKind::Budget)
                });
                finish!(EventBody::default());
            }
            emit_body!(EventBody {
                step: Some(step.clone()),
                reason: Some(reason.clone()),
                retrying: true,
                ..kind_of(EventKind::StepFailed)
            });
            result = run_once();
            if let StepResult::Failed { reason, .. } = &result {
                emit_body!(EventBody {
                    step: Some(step.clone()),
                    reason: Some(reason.clone()),
                    ..kind_of(EventKind::StepFailed)
                });
                candidates.clear();
                finish!(EventBody {
                    fallback: true,
                    ..EventBody::default()
                });
            }
        }
        let (records, timed_out) = match result {
            StepResult::Cancelled(_) => {
                if control.stopped() {
                    finish!(EventBody {
                        stopped: true,
                        ..EventBody::default()
                    });
                }
                emit_body!(EventBody {
                    step: Some(step),
                    skipped: true,
                    ..kind_of(EventKind::StepDone)
                });
                continue;
            }
            StepResult::Completed(records) => (records, false),
            StepResult::Timeout(records) => (records, true),
            StepResult::Failed { records, .. } => (records, false),
        };

        let sources: Vec<String> = read.digests.iter().map(|d| d.session_key.clone()).collect();
        let mut facts_capped = false;
        for record in records {
            let key = fact_key(&record);
            if let Some(existing) = candidates.iter_mut().find(|c| c.key == key) {
                for source in &sources {
                    if !existing.sources.contains(source) {
                        existing.sources.push(source.clone());
                    }
                }
            } else if candidates.len() >= budget.max_facts {
                facts_capped = true;
            } else if sources.len() >= budget.min_source_sessions {
                let candidate = Candidate {
                    key,
                    id: format!("fact-{}", candidates.len() + 1),
                    fact: record,
                    sources: sources.clone(),
                };
                let public = candidate.public();
                candidates.push(candidate);
                emit_body!(EventBody {
                    fact: Some(public),
                    ..kind_of(EventKind::Fact)
                });
            }
        }
        if candidates.len() >= budget.max_facts {
            facts_capped = true;
        }
        let mut reached: Vec<Kind> = Vec::new();
        if timed_out {
            reached.push(if elapsed() >= budget.total_ms {
                Kind::TotalMs
            } else {
                Kind::StepMs
            });
        }
        if tool_calls.load(Ordering::SeqCst) >= budget.tool_calls {
            reached.push(Kind::ToolCalls);
        }
        if read.truncated {
            reached.push(Kind::ReadBytes);
        }
        if facts_capped {
            reached.push(Kind::MaxFacts);
        }
        for kind in &reached {
            emit_body!(EventBody {
                budget: Some(*kind),
                ..kind_of(EventKind::Budget)
            });
        }
        emit_body!(EventBody {
            step: Some(step),
            ..kind_of(EventKind::StepDone)
        });
        if reached.iter().any(|kind| *kind != Kind::StepMs) {
            finish!(EventBody::default());
        }
    }
    finish!(EventBody::default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::AgentSessionSource;
    use crate::understand_step::{FactsStepLaunch, FactsStepLauncher};

    fn digest(key: &str, cwd: &str, at: &str) -> SessionDigest {
        SessionDigest {
            session_key: key.into(),
            source: AgentSessionSource::Codex,
            provider_session_id: key.into(),
            cwd: cwd.into(),
            title: "t".into(),
            model: None,
            started_at: at.into(),
            last_active_at: at.into(),
            message_count: 1,
            first_request: "hi".into(),
            tools_used: vec![],
            files_changed: vec![],
            outcome: String::new(),
            truncated: false,
        }
    }

    struct Records(&'static str);
    impl FactsStepLauncher for Records {
        fn launch(&self, launch: &FactsStepLaunch<'_>) -> Result<bool, String> {
            std::fs::write(launch.run_dir.join("extraction.jsonl"), self.0)
                .map_err(|e| e.to_string())?;
            std::fs::write(
                launch.run_dir.join("outcome.json"),
                "{\"childExit\":{\"code\":0}}",
            )
            .map_err(|e| e.to_string())?;
            (launch.on_call)();
            Ok(false)
        }
    }
    struct Failing;
    impl FactsStepLauncher for Failing {
        fn launch(&self, _: &FactsStepLaunch<'_>) -> Result<bool, String> {
            Err("boom".into())
        }
    }

    struct NoRegistry;
    impl maho_ext_api::ModelRegistry for NoRegistry {
        fn get_all(&self) -> Vec<maho_ext_api::Model> {
            Vec::new()
        }
        fn get_available(&self) -> Vec<maho_ext_api::Model> {
            Vec::new()
        }
        fn find(&self, _: &str, _: &str) -> Option<maho_ext_api::Model> {
            None
        }
        fn has_configured_auth(&self, _: &maho_ext_api::Model) -> bool {
            false
        }
        fn get_api_key_for_provider<'a>(
            &'a self,
            _: &'a str,
        ) -> maho_ext_api::ExtensionFuture<'a, Option<String>> {
            Box::pin(async { Ok(None) })
        }
    }

    fn input(
        launcher: Arc<dyn FactsStepLauncher>,
        digests: Vec<SessionDigest>,
    ) -> OnboardingUnderstandingInput {
        OnboardingUnderstandingInput {
            understand_id: "u".into(),
            scan_id: "s".into(),
            digests,
            project_roots: HashSet::new(),
            model: UnderstandingModel {
                model: "p/m".into(),
                model_registry: Arc::new(NoRegistry),
                env: BTreeMap::new(),
                launcher,
            },
            control: Arc::new(OnboardingUnderstandingControl::new()),
            budget: None,
            env: None,
            today: Some("2026-10-22".into()),
            now: None,
        }
    }

    const FACT: &str = "{\"scope\":\"project\",\"text\":\"uses rust\",\"date\":\"2026-10-22\"}\n";

    #[test]
    fn a_project_step_proposes_candidates_and_ends_with_done() {
        let digests = vec![
            digest("a", "/p", "2026-01-02T00:00:00.000Z"),
            digest("b", "/p", "2026-01-01T00:00:00.000Z"),
        ];
        let mut events = Vec::new();
        let done =
            run_onboarding_understanding(input(Arc::new(Records(FACT)), digests), &mut |e| {
                events.push(e)
            });
        let kinds: Vec<EventKind> = events.iter().map(|e| e.kind).collect();
        assert_eq!(
            kinds,
            vec![
                EventKind::StepStarted,
                EventKind::Fact,
                EventKind::StepDone,
                EventKind::Done
            ]
        );
        let candidates = done.candidates.unwrap_or_default();
        assert_eq!(
            (
                candidates.len(),
                candidates[0].id.as_str(),
                candidates[0].sources.len()
            ),
            (1, "fact-1", 2)
        );
        assert_eq!(done.tool_calls, 1);
    }

    #[test]
    fn a_model_that_fails_twice_falls_back_to_the_scan_only() {
        let digests = vec![
            digest("a", "/p", "2026-01-02T00:00:00.000Z"),
            digest("b", "/p", "2026-01-01T00:00:00.000Z"),
        ];
        let mut events = Vec::new();
        let done = run_onboarding_understanding(input(Arc::new(Failing), digests), &mut |e| {
            events.push(e)
        });
        assert_eq!(done.fallback, Some(UnderstandFallback::ScanOnly));
        assert_eq!(done.candidates, Some(vec![]));
        let retrying: Vec<Option<bool>> = events
            .iter()
            .filter(|e| e.kind == EventKind::StepFailed)
            .map(|e| e.retrying)
            .collect();
        assert_eq!(retrying, vec![Some(true), None]);
    }

    #[test]
    fn a_stopped_run_ends_with_stopped_and_single_sessions_never_form_a_step() {
        let digests = vec![
            digest("a", "/p", "2026-01-02T00:00:00.000Z"),
            digest("b", "/p", "2026-01-01T00:00:00.000Z"),
        ];
        let run = input(Arc::new(Records(FACT)), digests);
        run.control.stop();
        assert_eq!(
            run_onboarding_understanding(run, &mut |_| {}).stopped,
            Some(true)
        );
        let lone = vec![digest("a", "/p", "2026-01-02T00:00:00.000Z")];
        let done = run_onboarding_understanding(input(Arc::new(Records(FACT)), lone), &mut |_| {});
        assert_eq!(done.candidates, Some(vec![]));
    }
}
