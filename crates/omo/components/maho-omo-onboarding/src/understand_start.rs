//! The understanding pass behind `omo.onboarding.understand` (upstream `understand-start.ts`): when the scan has
//! digests, the run starts on its own thread so the caller can answer with the id (and hold the control)
//! before the first event.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex, PoisonError},
    thread::JoinHandle,
    time::Instant,
};

use crate::scan_contract::OnboardingUnavailable;
use crate::session_history::SessionDigest;
use crate::understand_contract::{
    OnboardingUnderstandBudgetLimits, OnboardingUnderstandEvent, OnboardingUnderstandEventKind,
    UnderstandFallback,
};
use crate::understand_control::OnboardingUnderstandingControl;
use crate::understand_runtime::{OnboardingUnderstandingInput, run_onboarding_understanding};
use crate::understand_step::{FactsStepLauncher, UnderstandingModel};

/// Test and wiring seams: the facts launch path and a budget override.
#[derive(Clone)]
pub struct OnboardingUnderstandingSeams {
    pub launcher: Arc<dyn FactsStepLauncher>,
    pub budget: Option<OnboardingUnderstandBudgetLimits>,
}

pub struct OnboardingUnderstandingStart {
    pub scan_id: String,
    pub model: String,
    pub digests: Vec<SessionDigest>,
    /// Project roots the scan listed; AGENTS.md is read only inside these.
    pub project_roots: HashSet<String>,
    pub model_registry: Arc<dyn maho_ext_api::ModelRegistry>,
    pub env: Option<BTreeMap<String, String>>,
    pub seams: OnboardingUnderstandingSeams,
}

pub struct OnboardingUnderstandingStarted {
    pub understand_id: String,
    pub control: Arc<OnboardingUnderstandingControl>,
    /// Joins the run; yields the final `done` event.
    pub finished: JoinHandle<OnboardingUnderstandEvent>,
}

pub enum OnboardingUnderstandingStartResult {
    Started(OnboardingUnderstandingStarted),
    Unavailable(OnboardingUnavailable),
}

pub type EmitUnderstand = Arc<dyn Fn(OnboardingUnderstandEvent) + Send + Sync>;

pub fn start_onboarding_understanding(
    input: OnboardingUnderstandingStart,
    emit: EmitUnderstand,
) -> OnboardingUnderstandingStartResult {
    if input.digests.is_empty() {
        return OnboardingUnderstandingStartResult::Unavailable(OnboardingUnavailable::new(
            "This scan found no recent sessions to read.",
        ));
    }
    let understand_id = uuid::Uuid::new_v4().to_string();
    let control = Arc::new(OnboardingUnderstandingControl::new());
    let started_at = Instant::now();
    let usage = Arc::new(Mutex::new((0_usize, 0_u32)));
    let tracked_usage = Arc::clone(&usage);
    let tracked_emit = Arc::clone(&emit);
    let (id, scan_id) = (understand_id.clone(), input.scan_id.clone());
    let run_control = Arc::clone(&control);
    let finished = std::thread::spawn(move || {
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_onboarding_understanding(
                OnboardingUnderstandingInput {
                    understand_id: id.clone(),
                    scan_id: scan_id.clone(),
                    digests: input.digests,
                    project_roots: input.project_roots,
                    model: UnderstandingModel {
                        model: input.model,
                        model_registry: input.model_registry,
                        env: BTreeMap::new(),
                        launcher: Arc::clone(&input.seams.launcher),
                    },
                    control: run_control,
                    budget: input.seams.budget,
                    env: input.env,
                    today: None,
                    now: None,
                },
                &mut |event| {
                    *tracked_usage.lock().unwrap_or_else(PoisonError::into_inner) =
                        (event.bytes_read, event.tool_calls);
                    tracked_emit(event);
                },
            )
        }));
        run.unwrap_or_else(|panic| {
            let reason = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
                .unwrap_or_else(|| "understanding failed".into());
            let (bytes_read, tool_calls) = *usage.lock().unwrap_or_else(PoisonError::into_inner);
            let done = OnboardingUnderstandEvent {
                understand_id: id,
                scan_id,
                kind: OnboardingUnderstandEventKind::Done,
                step: None,
                fact: None,
                budget: None,
                bytes_read,
                tool_calls,
                elapsed_ms: u64::try_from(started_at.elapsed().as_millis()).unwrap_or(u64::MAX),
                reason: Some(reason),
                retrying: None,
                skipped: None,
                candidates: Some(Vec::new()),
                fallback: Some(UnderstandFallback::ScanOnly),
                stopped: None,
            };
            emit(done.clone());
            done
        })
    });
    OnboardingUnderstandingStartResult::Started(OnboardingUnderstandingStarted {
        understand_id,
        control,
        finished,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::understand_step::FactsStepLaunch;

    struct Panics;
    impl FactsStepLauncher for Panics {
        fn launch(&self, _: &FactsStepLaunch<'_>) -> Result<bool, String> {
            panic!("launcher exploded")
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

    fn start(digests: Vec<SessionDigest>) -> OnboardingUnderstandingStart {
        OnboardingUnderstandingStart {
            scan_id: "s".into(),
            model: "p/m".into(),
            digests,
            project_roots: HashSet::new(),
            model_registry: Arc::new(NoRegistry),
            env: None,
            seams: OnboardingUnderstandingSeams {
                launcher: Arc::new(Panics),
                budget: None,
            },
        }
    }

    #[test]
    fn no_digests_means_unavailable() {
        match start_onboarding_understanding(start(vec![]), Arc::new(|_| {})) {
            OnboardingUnderstandingStartResult::Unavailable(u) => {
                assert_eq!(u.reason, "This scan found no recent sessions to read.")
            }
            OnboardingUnderstandingStartResult::Started(_) => panic!("started without digests"),
        }
    }

    #[test]
    fn a_run_that_panics_emits_done_with_the_scan_only_fallback() {
        use crate::session_history::AgentSessionSource;
        let digest = |key: &str| SessionDigest {
            session_key: key.into(),
            source: AgentSessionSource::Codex,
            provider_session_id: key.into(),
            cwd: "/p".into(),
            title: "t".into(),
            model: None,
            started_at: "2026-01-01T00:00:00.000Z".into(),
            last_active_at: "2026-01-01T00:00:00.000Z".into(),
            message_count: 1,
            first_request: "hi".into(),
            tools_used: vec![],
            files_changed: vec![],
            outcome: String::new(),
            truncated: false,
        };
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        let OnboardingUnderstandingStartResult::Started(started) = start_onboarding_understanding(
            start(vec![digest("a"), digest("b")]),
            Arc::new(move |e| sink.lock().unwrap_or_else(PoisonError::into_inner).push(e)),
        ) else {
            panic!("did not start");
        };
        let done = started.finished.join().expect("run thread");
        assert_eq!(
            (done.fallback, done.candidates),
            (Some(UnderstandFallback::ScanOnly), Some(vec![]))
        );
        assert!(done.reason.is_some_and(|r| r.contains("launcher exploded")));
        assert_eq!(
            seen.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .last()
                .map(|e| e.kind),
            Some(OnboardingUnderstandEventKind::Done)
        );
    }
}
