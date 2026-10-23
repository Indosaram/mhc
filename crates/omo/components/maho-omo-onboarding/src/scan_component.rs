//! `omo.onboarding.scan` / `omo.onboarding.apply` (upstream `scan-component.ts`): `omo setup`'s detection
//! streamed to RPC clients and its writers run on their consent. Programmatic only: no model is read or
//! called, no socket is opened, and nothing is written before an apply names its stages.
//!
//! Retained scans and the last engine credential store live in one process-wide [`OnboardingScanStore`]: a
//! client may scan on one control session and apply on the next, so nothing here lives per session.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, PoisonError},
};

use maho_ext_api::{
    EventBus, Extension, ExtensionApi, ExtensionFailure, ExtensionFuture, JsonValue,
};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::scan_contract::{
    ONBOARDING_APPLY_RPC_METHOD, ONBOARDING_SCAN_RPC_EVENT, ONBOARDING_SCAN_RPC_METHOD,
    OnboardingApplyResponse, OnboardingApplyStage, OnboardingScanAll, OnboardingScanEvent,
    OnboardingScanEventKind, OnboardingScanEventSource,
};
use crate::scan_runtime::{
    OnboardingEngineAuthStore, OnboardingScanEnvironment, OnboardingScanOptions,
    OnboardingScanOutcome, apply_retained_scan, run_onboarding_scan,
};
use crate::understand_component::{
    OnboardingUnderstandingHandlers, UnderstandRegistration,
    create_onboarding_understanding_handlers,
};
use crate::understand_contract::{
    ONBOARDING_UNDERSTAND_RPC_METHOD, ONBOARDING_UNDERSTAND_SKIP_RPC_METHOD,
    ONBOARDING_UNDERSTAND_STOP_RPC_METHOD,
};
use crate::understand_start::OnboardingUnderstandingSeams;

/// Finished scans an apply can still use; older ones answer `unavailable`.
pub const RETAINED_SCANS: usize = 8;
/// A scan describes the machine only briefly; after this, an apply answers stale instead of writing.
pub const ONBOARDING_SCAN_TTL_MS: i64 = 10 * 60_000;

pub type ScanOutcomeSlot = Option<Option<Arc<OnboardingScanOutcome>>>;

/// One retained scan: its outcome resolves when the scan settles (`Some(None)` for a failed scan).
pub struct HeldScan {
    pub finished_at: Mutex<Option<i64>>,
    pub outcome: watch::Receiver<ScanOutcomeSlot>,
}

impl HeldScan {
    pub fn finished_at(&self) -> Option<i64> {
        *self
            .finished_at
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Waits for the scan to settle: its outcome, or `None` when it failed before planning anything.
    pub async fn settled(&self) -> Option<Arc<OnboardingScanOutcome>> {
        let mut receiver = self.outcome.clone();
        let slot = receiver
            .wait_for(Option::is_some)
            .await
            .ok()
            .map(|slot| slot.clone());
        slot.flatten().flatten()
    }
}

#[derive(Default)]
struct StoreInner {
    /// Scan ids in insertion order.
    order: Vec<String>,
    scans: HashMap<String, Arc<HeldScan>>,
}

/// Retained scans, scans with an understanding run in progress, and the last engine credential store.
#[derive(Default)]
pub struct OnboardingScanStore {
    inner: Mutex<StoreInner>,
    /// Scans with an understanding run in progress, whichever session started it.
    understanding: Mutex<HashMap<String, u64>>,
    latest_auth_store: Mutex<Option<Arc<dyn OnboardingEngineAuthStore>>>,
}

pub fn create_onboarding_scan_store() -> Arc<OnboardingScanStore> {
    Arc::new(OnboardingScanStore::default())
}

/// The one store every session of this process shares.
pub fn process_scan_store() -> Arc<OnboardingScanStore> {
    static STORE: OnceLock<Arc<OnboardingScanStore>> = OnceLock::new();
    Arc::clone(STORE.get_or_init(create_onboarding_scan_store))
}

pub fn scan_expired(held: &HeldScan, now_ms: i64) -> bool {
    held.finished_at()
        .is_some_and(|finished| now_ms - finished > ONBOARDING_SCAN_TTL_MS)
}

impl OnboardingScanStore {
    pub fn get(&self, scan_id: &str) -> Option<Arc<HeldScan>> {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .scans
            .get(scan_id)
            .cloned()
    }

    pub fn remove(&self, scan_id: &str) -> Option<Arc<HeldScan>> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.order.retain(|id| id != scan_id);
        inner.scans.remove(scan_id)
    }

    /// Drops `scan_id` only while it still maps to `held`.
    pub fn remove_if_same(&self, scan_id: &str, held: &Arc<HeldScan>) {
        let same = self
            .get(scan_id)
            .is_some_and(|current| Arc::ptr_eq(&current, held));
        if same {
            self.remove(scan_id);
        }
    }

    pub fn len(&self) -> usize {
        self.inner
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .scans
            .len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Retains a new scan, drops expired ones, and keeps at most [`RETAINED_SCANS`] finished (or this) scans.
    pub fn insert(&self, scan_id: &str, held: Arc<HeldScan>, now_ms: i64) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        inner.order.retain(|id| id != scan_id);
        inner.scans.insert(scan_id.to_owned(), Arc::clone(&held));
        inner.order.push(scan_id.to_owned());
        let expired: Vec<String> = inner
            .order
            .iter()
            .filter(|id| {
                id.as_str() != scan_id
                    && inner
                        .scans
                        .get(*id)
                        .is_some_and(|entry| scan_expired(entry, now_ms))
            })
            .cloned()
            .collect();
        for id in &expired {
            inner.scans.remove(id);
        }
        inner.order.retain(|id| !expired.contains(id));
        let retained: Vec<String> = inner
            .order
            .iter()
            .filter(|id| {
                id.as_str() == scan_id
                    || inner
                        .scans
                        .get(*id)
                        .is_some_and(|entry| entry.finished_at().is_some())
            })
            .cloned()
            .collect();
        let excess = retained.len().saturating_sub(RETAINED_SCANS);
        for id in retained.into_iter().take(excess) {
            inner.scans.remove(&id);
            inner.order.retain(|other| *other != id);
        }
    }

    pub fn understanding_busy(&self, scan_id: &str) -> bool {
        self.understanding
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains_key(scan_id)
    }

    pub fn set_understanding(&self, scan_id: &str, token: u64) {
        self.understanding
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(scan_id.to_owned(), token);
    }

    pub fn release_understanding(&self, scan_id: &str, token: u64) {
        let mut busy = self
            .understanding
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if busy.get(scan_id) == Some(&token) {
            busy.remove(scan_id);
        }
    }

    pub fn latest_auth_store(&self) -> Option<Arc<dyn OnboardingEngineAuthStore>> {
        self.latest_auth_store
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set_latest_auth_store(&self, store: Arc<dyn OnboardingEngineAuthStore>) {
        *self
            .latest_auth_store
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(store);
    }
}

pub type AuthStoreSource =
    Arc<dyn Fn() -> Option<Arc<dyn OnboardingEngineAuthStore>> + Send + Sync>;

/// Dependencies of [`OnboardingScanComponent`]; every field defaults to the real thing.
#[derive(Clone, Default)]
pub struct OnboardingScanComponentDependencies {
    pub environment: Option<Arc<dyn Fn() -> OnboardingScanEnvironment + Send + Sync>>,
    /// Fake-model seams for understanding; without a facts launcher understanding answers unavailable.
    pub understanding: Option<OnboardingUnderstandingSeams>,
    /// The engine's credential store apply writes through; defaults to the one the process captured last.
    pub auth_store: Option<AuthStoreSource>,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
    /// Where finished scans wait for their apply; defaults to the one store every session shares.
    pub scan_store: Option<Arc<OnboardingScanStore>>,
}

#[derive(Default)]
pub struct OnboardingScanComponent {
    pub dependencies: OnboardingScanComponentDependencies,
}

impl OnboardingScanComponent {
    pub fn new(dependencies: OnboardingScanComponentDependencies) -> Self {
        Self { dependencies }
    }
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

fn emit_event(events: &EventBus, event: &OnboardingScanEvent) {
    if let Ok(data) = serde_json::to_value(event) {
        events.emit(
            "senpi:extension-rpc-event",
            &json!({"name": ONBOARDING_SCAN_RPC_EVENT, "data": data}),
        );
    }
}

fn panic_reason(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| (*s).to_owned()))
        .unwrap_or_else(|| "the scan failed".into())
}

fn json_handler(
    handler: impl Fn(
        JsonValue,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = JsonValue> + Send>>
    + Send
    + Sync
    + 'static,
) -> maho_ext_api::ExtensionRpcRequestHandler {
    Arc::new(move |request| -> ExtensionFuture<'static, JsonValue> {
        let future = handler(request);
        Box::pin(async move { Ok(future.await) })
    })
}

impl Extension for OnboardingScanComponent {
    fn register(&self, api: &mut ExtensionApi) {
        if let Err(error) = self.register_rpc(api) {
            maho_ext_api::diagnostic(&format!(
                "onboarding scan registration failed: {}",
                error.message
            ));
        }
    }
}

impl OnboardingScanComponent {
    fn register_rpc(&self, api: &mut ExtensionApi) -> Result<(), ExtensionFailure> {
        let deps = self.dependencies.clone();
        let environment = deps
            .environment
            .unwrap_or_else(|| Arc::new(OnboardingScanEnvironment::default));
        let now = deps.now.unwrap_or_else(|| Arc::new(now_ms));
        let shared = deps.scan_store.unwrap_or_else(process_scan_store);
        let model_registry: Arc<Mutex<Option<Arc<dyn maho_ext_api::ModelRegistry>>>> =
            Arc::new(Mutex::new(None));
        let auth_store = deps.auth_store.unwrap_or_else(|| {
            let shared = Arc::clone(&shared);
            Arc::new(move || shared.latest_auth_store())
        });

        let captured = Arc::clone(&model_registry);
        api.on(
            maho_ext_api::EventKind::SessionStart,
            Arc::new(move |_event, ctx| {
                let captured = Arc::clone(&captured);
                let registry = Arc::clone(&ctx.model_registry);
                Box::pin(async move {
                    *captured.lock().unwrap_or_else(PoisonError::into_inner) = Some(registry);
                    Ok(maho_ext_api::EventResult::None)
                })
            }),
        );

        // Understanding's handlers are built once per shell on first use, so its runs map scopes skip and stop.
        let handlers: Arc<Mutex<Option<Arc<OnboardingUnderstandingHandlers>>>> =
            Arc::new(Mutex::new(None));
        let registration = UnderstandRegistration {
            environment: Arc::clone(&environment),
            model_registry: {
                let model_registry = Arc::clone(&model_registry);
                Arc::new(move || {
                    model_registry
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .clone()
                })
            },
            store: Arc::clone(&shared),
            now: Arc::clone(&now),
            seams: deps.understanding.clone(),
            events: api.events.clone(),
        };
        let understanding = move || -> Arc<OnboardingUnderstandingHandlers> {
            let mut slot = handlers.lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(slot.get_or_insert_with(|| {
                Arc::new(create_onboarding_understanding_handlers(
                    registration.clone(),
                ))
            }))
        };
        for (method, pick) in [
            (ONBOARDING_UNDERSTAND_RPC_METHOD, 0_u8),
            (ONBOARDING_UNDERSTAND_SKIP_RPC_METHOD, 1),
            (ONBOARDING_UNDERSTAND_STOP_RPC_METHOD, 2),
        ] {
            let understanding = understanding.clone();
            api.rpc_handle(
                method,
                json_handler(move |request| {
                    let handlers = understanding();
                    Box::pin(async move {
                        match pick {
                            0 => handlers.understand(request).await,
                            1 => handlers.skip(&request),
                            _ => handlers.stop(&request),
                        }
                    })
                }),
            )?;
        }

        let events = api.events.clone();
        let scan_shared = Arc::clone(&shared);
        let scan_env = Arc::clone(&environment);
        let scan_now = Arc::clone(&now);
        api.rpc_handle(
            ONBOARDING_SCAN_RPC_METHOD,
            json_handler(move |request| {
                let (events, shared, environment, now) = (
                    events.clone(),
                    Arc::clone(&scan_shared),
                    Arc::clone(&scan_env),
                    Arc::clone(&scan_now),
                );
                Box::pin(async move {
                    let scan_id = uuid::Uuid::new_v4().to_string();
                    let include_custom_providers =
                        request.get("includeCustomProviders") == Some(&Value::Bool(true));
                    let (sender, receiver) = watch::channel::<ScanOutcomeSlot>(None);
                    let held = Arc::new(HeldScan {
                        finished_at: Mutex::new(None),
                        outcome: receiver,
                    });
                    shared.insert(&scan_id, Arc::clone(&held), now());
                    // The client learns the id from this response, so no event may precede it: the scan runs
                    // on its own thread. The clock is read when the final `done` event is emitted - inside the
                    // scan - so an apply racing in right after the events never sees a missing finish time.
                    let (thread_held, thread_id) = (Arc::clone(&held), scan_id.clone());
                    std::thread::spawn(move || {
                        let environment = environment();
                        let emitted_events = events.clone();
                        let finish_held = Arc::clone(&thread_held);
                        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            run_onboarding_scan(
                                &environment,
                                &OnboardingScanOptions {
                                    scan_id: thread_id.clone(),
                                    include_custom_providers,
                                    sessions: true,
                                },
                                &mut |event| {
                                    if event.kind == OnboardingScanEventKind::Done {
                                        *finish_held
                                            .finished_at
                                            .lock()
                                            .unwrap_or_else(PoisonError::into_inner) = Some(now());
                                    }
                                    emit_event(&emitted_events, &event);
                                },
                            )
                        }));
                        let slot = match run {
                            Ok(outcome) => Some(Some(Arc::new(outcome))),
                            Err(panic) => {
                                emit_event(
                                    &events,
                                    &OnboardingScanEvent {
                                        scan_id: thread_id,
                                        kind: OnboardingScanEventKind::Done,
                                        source: OnboardingScanEventSource::All(
                                            OnboardingScanAll::All,
                                        ),
                                        item: None,
                                        counts: None,
                                        elapsed_ms: 0,
                                        reason: Some(panic_reason(panic.as_ref())),
                                    },
                                );
                                Some(None)
                            }
                        };
                        let _ = sender.send(slot);
                    });
                    json!({"scanId": scan_id})
                })
            }),
        )?;

        let apply_shared = Arc::clone(&shared);
        let apply_now = Arc::clone(&now);
        api.rpc_handle(
            ONBOARDING_APPLY_RPC_METHOD,
            json_handler(move |request| {
                let (shared, now, auth_store) = (
                    Arc::clone(&apply_shared),
                    Arc::clone(&apply_now),
                    Arc::clone(&auth_store),
                );
                Box::pin(
                    async move { apply_response(&shared, now(), &request, &*auth_store).await },
                )
            }),
        )
    }
}

async fn apply_response(
    shared: &OnboardingScanStore,
    now_ms: i64,
    request: &Value,
    auth_store: &(dyn Fn() -> Option<Arc<dyn OnboardingEngineAuthStore>> + Send + Sync),
) -> Value {
    let respond =
        |response: OnboardingApplyResponse| serde_json::to_value(response).unwrap_or(Value::Null);
    let scan_id = request.get("scanId").and_then(Value::as_str);
    let stages = request.get("stages").and_then(Value::as_array);
    let (Some(scan_id), Some(stages)) = (scan_id, stages) else {
        return respond(OnboardingApplyResponse::invalid(
            "apply needs a scanId and a stages list",
        ));
    };
    let parsed: Vec<Option<OnboardingApplyStage>> = stages
        .iter()
        .map(|s| s.as_str().and_then(OnboardingApplyStage::parse))
        .collect();
    if parsed.iter().any(Option::is_none) {
        let unknown: Vec<String> = stages
            .iter()
            .zip(&parsed)
            .filter(|(_, stage)| stage.is_none())
            .map(|(raw, _)| raw.as_str().map_or_else(|| raw.to_string(), str::to_owned))
            .collect();
        return respond(OnboardingApplyResponse::invalid(format!(
            "unknown stages: {}",
            unknown.join(", ")
        )));
    }
    let requested: Vec<OnboardingApplyStage> = parsed.into_iter().flatten().collect();
    // One apply per scan: its plans describe the files as they were when the scan read them. Taking it out
    // of the store here, synchronously, means two applies racing on one scan never both run.
    let Some(held) = shared.remove(scan_id) else {
        return respond(OnboardingApplyResponse::unavailable(
            "No scan with this id; it finished long ago or was already applied.",
        ));
    };
    let Some(outcome) = held.settled().await else {
        return respond(OnboardingApplyResponse::unavailable(
            "The scan failed before it planned anything.",
        ));
    };
    if held.finished_at().is_none() || scan_expired(&held, now_ms) {
        return respond(OnboardingApplyResponse::stale(
            "The scan expired; retained scans live ten minutes. Run omo.onboarding.scan again.",
            Vec::new(),
            None,
        ));
    }
    let Some(plans) = outcome.plans.clone() else {
        return respond(OnboardingApplyResponse::Applied {
            scan_id: scan_id.to_owned(),
            applied: Vec::new(),
            notices: None,
        });
    };
    respond(apply_retained_scan(
        scan_id,
        plans.as_ref(),
        &requested,
        auth_store,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan_runtime::OnboardingScanEnvironment;

    fn api_with(deps: OnboardingScanComponentDependencies) -> ExtensionApi {
        let mut api = ExtensionApi::new(
            maho_ext_api::LoadedExtension::new(
                "scan",
                "/tmp".into(),
                maho_ext_api::SourceInfo::default(),
            ),
            maho_ext_api::ExtensionSessionProfile::default(),
            EventBus::default(),
            maho_ext_api::ExtensionRuntime::default(),
        );
        OnboardingScanComponent::new(deps).register(&mut api);
        api
    }

    fn deps(store: Arc<OnboardingScanStore>) -> OnboardingScanComponentDependencies {
        OnboardingScanComponentDependencies {
            environment: Some(Arc::new(|| OnboardingScanEnvironment {
                home: Some("/nonexistent-home".into()),
                env: Some(Default::default()),
                ..Default::default()
            })),
            scan_store: Some(store),
            ..Default::default()
        }
    }

    async fn call(api: &ExtensionApi, method: &str, request: Value) -> Value {
        let handler = api
            .registered
            .rpc_handlers
            .get(method)
            .cloned()
            .expect("handler registered");
        handler(request).await.expect("handler answers")
    }

    #[tokio::test]
    async fn apply_validates_before_touching_the_store() {
        let api = api_with(deps(create_onboarding_scan_store()));
        assert_eq!(
            call(&api, ONBOARDING_APPLY_RPC_METHOD, json!({})).await,
            json!({"kind":"invalid","reason":"apply needs a scanId and a stages list"})
        );
        assert_eq!(
            call(
                &api,
                ONBOARDING_APPLY_RPC_METHOD,
                json!({"scanId":"x","stages":["credentials","bogus",7]})
            )
            .await,
            json!({"kind":"invalid","reason":"unknown stages: bogus, 7"})
        );
        assert_eq!(
            call(
                &api,
                ONBOARDING_APPLY_RPC_METHOD,
                json!({"scanId":"x","stages":["assets"]})
            )
            .await,
            json!({"kind":"unavailable","reason":"No scan with this id; it finished long ago or was already applied."})
        );
    }

    #[tokio::test]
    async fn a_finished_scan_is_applied_once_and_an_old_one_is_stale() {
        let store = create_onboarding_scan_store();
        let api = api_with(deps(Arc::clone(&store)));
        let reply = call(&api, ONBOARDING_SCAN_RPC_METHOD, json!({})).await;
        let scan_id = reply["scanId"].as_str().expect("scanId").to_owned();
        let held = store.get(&scan_id).expect("retained");
        assert!(held.settled().await.is_some());
        assert!(held.finished_at().is_some());
        let first = call(
            &api,
            ONBOARDING_APPLY_RPC_METHOD,
            json!({"scanId":scan_id,"stages":[]}),
        )
        .await;
        assert_eq!(first["applied"], json!([]));
        let second = call(
            &api,
            ONBOARDING_APPLY_RPC_METHOD,
            json!({"scanId":scan_id,"stages":[]}),
        )
        .await;
        assert_eq!(second["kind"], "unavailable");

        // An expired scan answers stale instead of writing.
        let (sender, receiver) = watch::channel::<ScanOutcomeSlot>(None);
        let old = Arc::new(HeldScan {
            finished_at: Mutex::new(Some(0)),
            outcome: receiver,
        });
        let outcome = OnboardingScanOutcome {
            plans: None,
            results: vec![],
            digests: vec![],
            project_roots: Default::default(),
        };
        sender
            .send(Some(Some(Arc::new(outcome))))
            .expect("receiver alive");
        store.insert("old", old, 0);
        let stale = apply_response(
            &store,
            ONBOARDING_SCAN_TTL_MS + 1,
            &json!({"scanId":"old","stages":[]}),
            &|| None,
        )
        .await;
        assert_eq!(stale["kind"], "stale");
    }

    #[test]
    fn only_the_newest_finished_scans_are_retained() {
        let store = OnboardingScanStore::default();
        for index in 0..(RETAINED_SCANS + 3) {
            let (sender, receiver) = watch::channel::<ScanOutcomeSlot>(None);
            drop(sender);
            let held = Arc::new(HeldScan {
                finished_at: Mutex::new(Some(1)),
                outcome: receiver,
            });
            store.insert(&format!("scan-{index}"), held, 1);
        }
        assert_eq!(store.len(), RETAINED_SCANS);
        assert!(
            store.get("scan-0").is_none()
                && store.get(&format!("scan-{}", RETAINED_SCANS + 2)).is_some()
        );
    }
}
