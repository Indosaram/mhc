//! The three understanding RPC answers (upstream `understand-component.ts`); the scan shell registers the
//! method names and delegates here. One instance per registered shell: its runs map scopes skip and stop.

use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex, PoisonError,
        atomic::{AtomicU64, Ordering},
    },
};

use maho_ext_api::{EventBus, ModelRegistry};
use serde_json::{Value, json};

use crate::scan_component::{OnboardingScanStore, scan_expired};
use crate::scan_runtime::OnboardingScanEnvironment;
use crate::understand_contract::ONBOARDING_UNDERSTAND_RPC_EVENT;
use crate::understand_control::OnboardingUnderstandingControl;
use crate::understand_start::{
    OnboardingUnderstandingSeams, OnboardingUnderstandingStart, OnboardingUnderstandingStartResult,
    start_onboarding_understanding,
};

#[derive(Clone)]
pub struct UnderstandRegistration {
    pub environment: Arc<dyn Fn() -> OnboardingScanEnvironment + Send + Sync>,
    pub model_registry: Arc<dyn Fn() -> Option<Arc<dyn ModelRegistry>> + Send + Sync>,
    /// The shared store: retained scans, and the per-scan busy lock shared by every session of the process.
    pub store: Arc<OnboardingScanStore>,
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    pub seams: Option<OnboardingUnderstandingSeams>,
    pub events: EventBus,
}

pub struct OnboardingUnderstandingHandlers {
    registration: UnderstandRegistration,
    runs: Arc<Mutex<HashMap<String, Arc<OnboardingUnderstandingControl>>>>,
    tokens: AtomicU64,
}

pub fn create_onboarding_understanding_handlers(
    registration: UnderstandRegistration,
) -> OnboardingUnderstandingHandlers {
    OnboardingUnderstandingHandlers {
        registration,
        runs: Arc::default(),
        tokens: AtomicU64::new(1),
    }
}

fn invalid(reason: &str) -> Value {
    json!({"kind": "invalid", "reason": reason})
}

fn unavailable(reason: &str) -> Value {
    json!({"kind": "unavailable", "reason": reason})
}

/// `^[^/\s]+\/\S+$`
fn is_model_selector(model: &str) -> bool {
    let Some((provider, id)) = model.split_once('/') else {
        return false;
    };
    !provider.is_empty()
        && !provider.chars().any(char::is_whitespace)
        && !id.is_empty()
        && !id.chars().any(char::is_whitespace)
}

impl OnboardingUnderstandingHandlers {
    /// The scan's digests exist only in memory, with the retained scan: an expired, evicted or applied scan has none.
    async fn scan_for_understanding(
        &self,
        scan_id: &str,
    ) -> Option<Arc<crate::scan_runtime::OnboardingScanOutcome>> {
        let store = &self.registration.store;
        let held = store.get(scan_id)?;
        let outcome = held.settled().await?;
        if !store
            .get(scan_id)
            .is_some_and(|current| Arc::ptr_eq(&current, &held))
        {
            return None;
        }
        if held.finished_at().is_none() || scan_expired(&held, (self.registration.now)()) {
            store.remove(scan_id);
            return None;
        }
        Some(outcome)
    }

    pub async fn understand(&self, request: Value) -> Value {
        if request.get("consent") != Some(&Value::Bool(true)) {
            return invalid(
                "Understanding sends the scan's session digests and each scanned project's AGENTS.md to the model provider, so it needs consent: true.",
            );
        }
        let scan_id = match request.get("scanId").and_then(Value::as_str) {
            Some(id) if !id.is_empty() => id.to_owned(),
            _ => return invalid("understand needs the scanId of a scan"),
        };
        let model = match request.get("model").and_then(Value::as_str) {
            Some(model) if is_model_selector(model) => model.to_owned(),
            _ => return invalid("model must be provider/model"),
        };
        let Some(model_registry) = (self.registration.model_registry)() else {
            return unavailable("No session has started yet, so no model is available.");
        };
        let store = Arc::clone(&self.registration.store);
        if store.understanding_busy(&scan_id) {
            return invalid(
                "An understanding run is already in progress for this scan; stop it or wait for its done event.",
            );
        }
        let token = self.tokens.fetch_add(1, Ordering::SeqCst);
        store.set_understanding(&scan_id, token);
        let release = {
            let (store, scan_id) = (Arc::clone(&store), scan_id.clone());
            move || store.release_understanding(&scan_id, token)
        };
        let Some(scan) = self.scan_for_understanding(&scan_id).await else {
            release();
            return unavailable(
                "No retained scan with this id: it never ran, failed, expired after ten minutes, or was applied. Run omo.onboarding.scan again.",
            );
        };
        let Some(seams) = self.registration.seams.clone() else {
            release();
            return unavailable(
                "The onboarding understanding launcher is not wired in this build.",
            );
        };
        let environment = (self.registration.environment)();
        let events = self.registration.events.clone();
        let done_release = {
            let (store, scan_id) = (Arc::clone(&store), scan_id.clone());
            move || store.release_understanding(&scan_id, token)
        };
        let started = start_onboarding_understanding(
            OnboardingUnderstandingStart {
                scan_id: scan_id.clone(),
                model,
                digests: scan.digests.clone(),
                project_roots: scan.project_roots.clone(),
                model_registry,
                env: environment.env,
                seams,
            },
            Arc::new(move |event| {
                if event.kind == crate::understand_contract::OnboardingUnderstandEventKind::Done {
                    done_release();
                }
                if let Ok(data) = serde_json::to_value(&event) {
                    events.emit(
                        "senpi:extension-rpc-event",
                        &json!({"name": ONBOARDING_UNDERSTAND_RPC_EVENT, "data": data}),
                    );
                }
            }),
        );
        match started {
            OnboardingUnderstandingStartResult::Unavailable(reply) => {
                release();
                serde_json::to_value(reply).unwrap_or(Value::Null)
            }
            OnboardingUnderstandingStartResult::Started(started) => {
                let understand_id = started.understand_id.clone();
                self.runs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(understand_id.clone(), Arc::clone(&started.control));
                let runs = Arc::clone(&self.runs);
                let finished_id = understand_id.clone();
                std::thread::spawn(move || {
                    let _ = started.finished.join();
                    runs.lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .remove(&finished_id);
                    release();
                });
                json!({"understandId": understand_id})
            }
        }
    }

    fn control(
        &self,
        request: &Value,
        act: impl FnOnce(&OnboardingUnderstandingControl, &Value) -> Option<&'static str>,
    ) -> Value {
        let Some(understand_id) = request.get("understandId").and_then(Value::as_str) else {
            return invalid("needs an understandId");
        };
        let run = self
            .runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(understand_id)
            .cloned();
        let Some(run) = run else {
            return unavailable("No running understanding with this id.");
        };
        match act(&run, request) {
            None => json!({"understandId": understand_id, "accepted": true}),
            Some(reason) => invalid(reason),
        }
    }

    pub fn skip(&self, request: &Value) -> Value {
        self.control(request, |run, body| {
            match body.get("project").and_then(Value::as_str) {
                Some(project) if !project.is_empty() => {
                    run.skip(project);
                    None
                }
                _ => Some("skip needs a project directory"),
            }
        })
    }

    pub fn stop(&self, request: &Value) -> Value {
        self.control(request, |run, _| {
            run.stop();
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct NoRegistry;
    impl ModelRegistry for NoRegistry {
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

    fn handlers(with_registry: bool) -> OnboardingUnderstandingHandlers {
        create_onboarding_understanding_handlers(UnderstandRegistration {
            environment: Arc::new(OnboardingScanEnvironment::default),
            model_registry: Arc::new(move || {
                with_registry.then(|| Arc::new(NoRegistry) as Arc<dyn ModelRegistry>)
            }),
            store: crate::scan_component::create_onboarding_scan_store(),
            now: Arc::new(|| 0),
            seams: None,
            events: EventBus::default(),
        })
    }

    #[test]
    fn the_model_selector_is_provider_slash_model() {
        assert!(is_model_selector("anthropic/claude-x"));
        for bad in ["claude", "/m", "p/", "p /m", "p/m x"] {
            assert!(!is_model_selector(bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn requests_are_validated_in_the_documented_order() {
        let h = handlers(false);
        assert_eq!(
            h.understand(json!({"scanId":"s","model":"p/m"})).await["kind"],
            "invalid"
        );
        let no_scan = h.understand(json!({"consent":true,"model":"p/m"})).await;
        assert_eq!(no_scan["reason"], "understand needs the scanId of a scan");
        let bad_model = h
            .understand(json!({"consent":true,"scanId":"s","model":"nope"}))
            .await;
        assert_eq!(bad_model["reason"], "model must be provider/model");
        let no_session = h
            .understand(json!({"consent":true,"scanId":"s","model":"p/m"}))
            .await;
        assert_eq!(
            no_session["reason"],
            "No session has started yet, so no model is available."
        );
        let missing = handlers(true)
            .understand(json!({"consent":true,"scanId":"s","model":"p/m"}))
            .await;
        assert_eq!(
            missing["reason"],
            "No retained scan with this id: it never ran, failed, expired after ten minutes, or was applied. Run omo.onboarding.scan again."
        );
    }

    #[test]
    fn control_calls_need_a_running_understanding() {
        let h = handlers(true);
        assert_eq!(h.stop(&json!({})), invalid("needs an understandId"));
        assert_eq!(
            h.stop(&json!({"understandId":"u"})),
            unavailable("No running understanding with this id.")
        );
        let control = Arc::new(OnboardingUnderstandingControl::new());
        h.runs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert("u".into(), Arc::clone(&control));
        assert_eq!(
            h.skip(&json!({"understandId":"u"})),
            invalid("skip needs a project directory")
        );
        assert_eq!(
            h.skip(&json!({"understandId":"u","project":"/p"})),
            json!({"understandId":"u","accepted":true})
        );
        assert!(control.is_skipped("/p"));
        assert_eq!(h.stop(&json!({"understandId":"u"}))["accepted"], true);
        assert!(control.stopped());
    }
}
