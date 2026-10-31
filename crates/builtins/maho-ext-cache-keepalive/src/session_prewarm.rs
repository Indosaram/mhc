//! Port of `cache-keepalive/session-prewarm.ts` (senpi 903b8336).
//!
//! Arm at session start, then prewarm once on the first draft edit (senpi#3140).
//! A submitted prompt consumes the opportunity: the real request warms its own prefix.
//! The request is the host's `getPromptCachePrefixRequest()` prefix, the first user turn's
//! request with an empty conversation, because the platform reuses a prefix only up to a
//! block boundary: a prewarmed system prompt that stops short of the turn's is never read.
//! When the host cannot build that prefix without side effects (an extension handler that is
//! not preview-safe, or a user prompt that already started its turn), a `skipped` entry
//! records why (senpi#2115).
//! The request runs detached from the turn pipeline, so it can never delay or fail the
//! first user turn; its billed usage is recorded as a custom entry that session stats count.
//!
//! Host seams: the context is the [`PrewarmContext`] trait, implemented for the live
//! `ExtensionContext` by [`ExtensionPrewarmContext`] over `on_user_activity` and
//! `get_prompt_cache_prefix_request`; the `pi.on(model_select | session_parked |
//! session_resumed | session_shutdown)` handlers are the `on_*` methods, which the extension
//! wiring calls from those events.

use crate::prewarm_entry::{PROMPT_CACHE_PREWARM_ENTRY_TYPE, PromptCachePrewarmEntryData};
use maho_ai::{
    api::{
        openai_responses_prompt_cache::is_openai_responses_prompt_cache_model,
        warm_prompt_cache::{WarmPromptCacheOptions, WarmPromptCacheResult, WarmPromptCacheUsage},
    },
    models::model_cost::cost_card_at,
    types::{BoxFuture, CacheRetention, Context, Model, Usage, UsageCost},
    utils::{
        abort::{AbortController, AbortSignal},
        diagnostics::now_ms,
    },
};
use serde_json::Value;
use std::sync::{Arc, Mutex, PoisonError};

pub const PROMPT_CACHE_PREWARM_TIMEOUT_MS: u64 = 30_000;

pub use maho_ext_api::UserActivityKind;
pub type ActivityListener = maho_ext_api::UserActivityListener;
pub type Unsubscribe = maho_ext_api::UserActivityUnsubscribe;

/// The ready prefix request: `{ model, context, options }`.
pub struct PromptCachePrefixRequest {
    pub model: Model,
    pub context: Context,
    pub options: WarmPromptCacheOptions,
}

/// `getPromptCachePrefixRequest` result.
pub enum PromptCachePrefixOutcome {
    Ready(PromptCachePrefixRequest),
    Skipped(String),
}

/// The slice of `ExtensionContext` a session prewarm reads.
pub trait PrewarmContext: Send + Sync {
    fn model(&self) -> Option<Model>;
    /// `getPromptCacheKeepAliveSettings?.().enabled === true`.
    fn keep_alive_enabled(&self) -> bool;
    fn is_idle(&self) -> bool;
    fn has_pending_messages(&self) -> bool;
    /// `getPromptCachePrefixRequest?.({ signal })`; `None` when the host has no prefix builder.
    fn prompt_cache_prefix_request(
        &self,
        signal: AbortSignal,
    ) -> BoxFuture<'static, Option<PromptCachePrefixOutcome>>;
    /// `onUserActivity?.(listener)`; `None` when the host cannot report activity.
    fn on_user_activity(&self, listener: ActivityListener) -> Option<Unsubscribe>;
}

/// The live `ExtensionContext` as a [`PrewarmContext`]: every read goes through the context's
/// actions, so a retired context reports no model, no keep-alive and no activity.
pub struct ExtensionPrewarmContext(pub maho_ext_api::ExtensionContext);

impl PrewarmContext for ExtensionPrewarmContext {
    fn model(&self) -> Option<Model> {
        self.0.current_model().ok().flatten()
    }
    fn keep_alive_enabled(&self) -> bool {
        self.0
            .get_prompt_cache_keep_alive_settings()
            .is_ok_and(|settings| settings.enabled)
    }
    fn is_idle(&self) -> bool {
        self.0.is_idle()
    }
    fn has_pending_messages(&self) -> bool {
        self.0.has_pending_messages().unwrap_or(true)
    }
    fn prompt_cache_prefix_request(
        &self,
        signal: AbortSignal,
    ) -> BoxFuture<'static, Option<PromptCachePrefixOutcome>> {
        let ctx = self.0.clone();
        Box::pin(async move {
            let options = maho_ext_api::PromptCachePrefixRequestOptions {
                sizes_only: false,
                signal: Some(signal),
            };
            // A context retired mid-build ends the attempt: the session it would warm is gone.
            match ctx.get_prompt_cache_prefix_request(options).await.ok()? {
                maho_ext_api::PromptCachePrefixResult::Ready(request) => {
                    let request = *request;
                    Some(PromptCachePrefixOutcome::Ready(PromptCachePrefixRequest {
                        model: request.model,
                        context: request.context,
                        options: request.options,
                    }))
                }
                maho_ext_api::PromptCachePrefixResult::Skipped(reason) => {
                    Some(PromptCachePrefixOutcome::Skipped(reason))
                }
            }
        })
    }
    fn on_user_activity(&self, listener: ActivityListener) -> Option<Unsubscribe> {
        self.0.on_user_activity(listener).ok().flatten()
    }
}

pub type WarmPromptCacheFn = Arc<
    dyn Fn(
            Model,
            Context,
            WarmPromptCacheOptions,
        ) -> BoxFuture<'static, Result<WarmPromptCacheResult, String>>
        + Send
        + Sync,
>;
pub type IsPrewarmModel = Arc<dyn Fn(&Model) -> bool + Send + Sync>;
/// `pi.appendEntry(customType, data)`.
pub type AppendEntry = Arc<dyn Fn(&str, Value) + Send + Sync>;

pub struct SessionPrewarmDependencies {
    pub warm: WarmPromptCacheFn,
    pub is_prewarm_model: Option<IsPrewarmModel>,
}

#[derive(Default)]
struct State {
    current: Option<AbortController>,
    context: Option<Arc<dyn PrewarmContext>>,
    used: bool,
    submitted: bool,
    parked: bool,
    unsubscribe_activity: Option<Unsubscribe>,
}

struct Inner {
    state: Mutex<State>,
    warm: WarmPromptCacheFn,
    is_prewarm_model: IsPrewarmModel,
    append_entry: AppendEntry,
}

#[derive(Clone)]
pub struct SessionPrewarm {
    inner: Arc<Inner>,
}

pub fn create_session_prewarm(
    append_entry: AppendEntry,
    dependencies: SessionPrewarmDependencies,
) -> SessionPrewarm {
    SessionPrewarm {
        inner: Arc::new(Inner {
            state: Mutex::default(),
            warm: dependencies.warm,
            is_prewarm_model: dependencies
                .is_prewarm_model
                .unwrap_or_else(|| Arc::new(is_openai_responses_prompt_cache_model)),
            append_entry,
        }),
    }
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn cancel(&self) {
        if let Some(current) = self.lock().current.take() {
            current.abort(None);
        }
    }

    fn append(&self, data: PromptCachePrewarmEntryData) {
        (self.append_entry)(PROMPT_CACHE_PREWARM_ENTRY_TYPE, data.to_json());
    }

    fn finish(&self, controller: &AbortController) {
        let mut state = self.lock();
        if state
            .current
            .as_ref()
            .is_some_and(|current| current.signal().same_signal(&controller.signal()))
        {
            state.current = None;
        }
    }

    async fn run(
        self: Arc<Self>,
        ctx: Arc<dyn PrewarmContext>,
        model: Model,
        controller: AbortController,
    ) {
        let signal = controller.signal();
        let outcome = self.attempt(&ctx, &model, &signal).await;
        if let Err(message) = outcome
            && !signal.aborted()
        {
            self.append(PromptCachePrewarmEntryData::Failed {
                provider: model.provider.clone(),
                model: model.id.clone(),
                error: message,
            });
        }
        self.finish(&controller);
    }

    async fn attempt(
        &self,
        ctx: &Arc<dyn PrewarmContext>,
        model: &Model,
        signal: &AbortSignal,
    ) -> Result<(), String> {
        let prefix = ctx.prompt_cache_prefix_request(signal.clone()).await;
        let Some(prefix) = prefix else { return Ok(()) };
        if signal.aborted() {
            return Ok(());
        }
        let request = match prefix {
            PromptCachePrefixOutcome::Skipped(reason) => {
                self.append(PromptCachePrewarmEntryData::Skipped {
                    provider: model.provider.clone(),
                    model: model.id.clone(),
                    reason,
                });
                return Ok(());
            }
            PromptCachePrefixOutcome::Ready(request) => request,
        };
        if self.lock().submitted || !ctx.keep_alive_enabled() {
            return Ok(());
        }
        let mut options = request.options;
        options.stream.request.signal = Some(signal.clone());
        options.stream.request.timeout_ms = Some(PROMPT_CACHE_PREWARM_TIMEOUT_MS);
        let result = (self.warm)(request.model, request.context, options).await?;
        if signal.aborted() {
            return Ok(());
        }
        let WarmPromptCacheResult::Supported { usage, .. } = result else {
            return Ok(());
        };
        self.append(PromptCachePrewarmEntryData::Warmed {
            provider: model.provider.clone(),
            model: model.id.clone(),
            usage: to_usage(model, &usage),
        });
        Ok(())
    }

    fn on_activity(self: &Arc<Self>, kind: UserActivityKind) {
        let (ctx, controller) = {
            let mut state = self.lock();
            if kind == UserActivityKind::Prompt {
                state.submitted = true;
                state.used = true;
                return;
            }
            let Some(ctx) = state.context.clone() else {
                return;
            };
            if state.parked || state.used {
                return;
            }
            state.used = true;
            (ctx, AbortController::new())
        };
        let Some(model) = ctx.model() else { return };
        if !ctx.keep_alive_enabled()
            || model.cache_retention == Some(CacheRetention::None)
            || !(self.is_prewarm_model)(&model)
            || !ctx.is_idle()
            || ctx.has_pending_messages()
        {
            return;
        }
        self.lock().current = Some(controller.clone());
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            self.finish(&controller);
            return;
        };
        runtime.spawn(self.clone().run(ctx, model, controller));
    }
}

impl SessionPrewarm {
    pub fn start(&self, ctx: Arc<dyn PrewarmContext>) {
        self.inner.cancel();
        let previous = {
            let mut state = self.inner.lock();
            state.context = Some(ctx.clone());
            state.used = false;
            state.submitted = false;
            state.parked = false;
            state.unsubscribe_activity.take()
        };
        if let Some(unsubscribe) = previous {
            unsubscribe();
        }
        let weak = Arc::downgrade(&self.inner);
        let unsubscribe = ctx.on_user_activity(Arc::new(move |kind| {
            if let Some(inner) = weak.upgrade() {
                inner.on_activity(kind);
            }
        }));
        self.inner.lock().unsubscribe_activity = unsubscribe;
    }

    pub fn cancel(&self) {
        self.inner.cancel();
    }

    /// `pi.on("model_select")`.
    pub fn on_model_select(&self, ctx: Arc<dyn PrewarmContext>) {
        self.inner.cancel();
        self.inner.lock().context = Some(ctx);
    }

    /// `pi.on("session_parked")`.
    pub fn on_session_parked(&self) {
        self.inner.lock().parked = true;
        self.inner.cancel();
    }

    /// `pi.on("session_resumed")`.
    pub fn on_session_resumed(&self, ctx: Arc<dyn PrewarmContext>) {
        let mut state = self.inner.lock();
        state.parked = false;
        state.context = Some(ctx);
    }

    /// `pi.on("session_shutdown")`.
    pub fn on_session_shutdown(&self) {
        let unsubscribe = {
            let mut state = self.inner.lock();
            state.context = None;
            state.unsubscribe_activity.take()
        };
        if let Some(unsubscribe) = unsubscribe {
            unsubscribe();
        }
        self.inner.cancel();
    }

    /// Whether a prewarm request is in flight.
    pub fn in_flight(&self) -> bool {
        self.inner.lock().current.is_some()
    }
}

fn to_usage(model: &Model, usage: &WarmPromptCacheUsage) -> Usage {
    let cost = usage.cost.unwrap_or_else(|| {
        let rates = cost_card_at(&model.cost, now_ms());
        UsageCost {
            input: usage.input as f64 * rates.input / 1_000_000.0,
            output: usage.output as f64 * rates.output / 1_000_000.0,
            cache_read: usage.cache_read as f64 * rates.cache_read / 1_000_000.0,
            cache_write: usage.cache_write as f64 * rates.cache_write / 1_000_000.0,
            total: 0.0,
        }
    });
    Usage {
        input: usage.input,
        output: usage.output,
        cache_read: usage.cache_read,
        cache_write: usage.cache_write,
        total_tokens: usage.input + usage.output + usage.cache_read + usage.cache_write,
        cost: UsageCost {
            total: cost.input + cost.output + cost.cache_read + cost.cache_write,
            ..cost
        },
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn model() -> Model {
        let mut model: Model = serde_json::from_value(json!({
            "id":"gpt","name":"gpt","api":"openai-responses","provider":"openai",
            "baseUrl":"https://api.openai.com/v1","reasoning":false,"input":[],
            "cost":{"input":2.0,"output":8.0,"cacheRead":0.5,"cacheWrite":1.0},
            "contextWindow":100,"maxTokens":10,
            "compat":{"supportsExplicitPromptCacheMode":true}
        }))
        .expect("model");
        model.cache_retention = None;
        model
    }

    struct Host {
        model: Mutex<Option<Model>>,
        enabled: AtomicBool,
        idle: AtomicBool,
        pending: AtomicBool,
        prefix: Mutex<Option<Box<dyn Fn() -> Option<PromptCachePrefixOutcome> + Send>>>,
        listeners: Mutex<Vec<ActivityListener>>,
    }

    impl Host {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                model: Mutex::new(Some(model())),
                enabled: AtomicBool::new(true),
                idle: AtomicBool::new(true),
                pending: AtomicBool::new(false),
                prefix: Mutex::new(Some(Box::new(|| {
                    Some(PromptCachePrefixOutcome::Ready(PromptCachePrefixRequest {
                        model: model(),
                        context: Context::default(),
                        options: WarmPromptCacheOptions::default(),
                    }))
                }))),
                listeners: Mutex::default(),
            })
        }
        fn fire(&self, kind: UserActivityKind) {
            let listeners: Vec<_> = self.listeners.lock().unwrap().clone();
            for listener in listeners {
                listener(kind);
            }
        }
    }

    impl PrewarmContext for Host {
        fn model(&self) -> Option<Model> {
            self.model.lock().unwrap().clone()
        }
        fn keep_alive_enabled(&self) -> bool {
            self.enabled.load(Ordering::SeqCst)
        }
        fn is_idle(&self) -> bool {
            self.idle.load(Ordering::SeqCst)
        }
        fn has_pending_messages(&self) -> bool {
            self.pending.load(Ordering::SeqCst)
        }
        fn prompt_cache_prefix_request(
            &self,
            _signal: AbortSignal,
        ) -> BoxFuture<'static, Option<PromptCachePrefixOutcome>> {
            let outcome = self
                .prefix
                .lock()
                .unwrap()
                .as_ref()
                .and_then(|build| build());
            Box::pin(async move { outcome })
        }
        fn on_user_activity(&self, listener: ActivityListener) -> Option<Unsubscribe> {
            self.listeners.lock().unwrap().push(listener);
            Some(Box::new(|| {}))
        }
    }

    struct Harness {
        entries: Arc<Mutex<Vec<(String, Value)>>>,
        warms: Arc<AtomicUsize>,
        done: Arc<tokio::sync::Notify>,
        prewarm: SessionPrewarm,
    }

    fn harness(result: Result<WarmPromptCacheResult, String>) -> Harness {
        let entries: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
        let warms = Arc::new(AtomicUsize::new(0));
        let done = Arc::new(tokio::sync::Notify::new());
        let recorded = entries.clone();
        let notify = done.clone();
        let append: AppendEntry = Arc::new(move |kind, data| {
            recorded.lock().unwrap().push((kind.to_owned(), data));
            notify.notify_one();
        });
        let counter = warms.clone();
        let warm: WarmPromptCacheFn = Arc::new(move |_, _, options| {
            counter.fetch_add(1, Ordering::SeqCst);
            assert_eq!(
                options.stream.request.timeout_ms,
                Some(PROMPT_CACHE_PREWARM_TIMEOUT_MS)
            );
            assert!(options.stream.request.signal.is_some());
            let result = result.clone();
            Box::pin(async move { result })
        });
        let prewarm = create_session_prewarm(
            append,
            SessionPrewarmDependencies {
                warm,
                is_prewarm_model: None,
            },
        );
        Harness {
            entries,
            warms,
            done,
            prewarm,
        }
    }

    fn supported() -> WarmPromptCacheResult {
        WarmPromptCacheResult::Supported {
            usage: WarmPromptCacheUsage {
                input: 1_000_000,
                output: 0,
                cache_read: 0,
                cache_write: 2_000_000,
                cost: None,
            },
            usage_raw: Value::Null,
        }
    }

    async fn settle(harness: &Harness) {
        tokio::time::timeout(std::time::Duration::from_secs(5), harness.done.notified())
            .await
            .expect("prewarm entry");
    }

    #[tokio::test]
    async fn first_draft_prewarms_once_and_records_usage() {
        let h = harness(Ok(supported()));
        let host = Host::new();
        h.prewarm.start(host.clone());
        host.fire(UserActivityKind::Draft);
        settle(&h).await;
        host.fire(UserActivityKind::Draft);
        assert_eq!(h.warms.load(Ordering::SeqCst), 1);
        let entries = h.entries.lock().unwrap().clone();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].0, PROMPT_CACHE_PREWARM_ENTRY_TYPE);
        let data = &entries[0].1;
        assert_eq!(data["phase"], "warmed");
        assert_eq!(data["provider"], "openai");
        assert_eq!(data["model"], "gpt");
        assert_eq!(data["usage"]["totalTokens"], 3_000_000);
        assert_eq!(data["usage"]["cost"]["total"], 4.0);
    }

    #[tokio::test]
    async fn submitted_prompt_consumes_the_opportunity() {
        let h = harness(Ok(supported()));
        let host = Host::new();
        h.prewarm.start(host.clone());
        host.fire(UserActivityKind::Prompt);
        host.fire(UserActivityKind::Draft);
        assert_eq!(h.warms.load(Ordering::SeqCst), 0);
        assert!(!h.prewarm.in_flight());
        // A fresh start re-arms.
        h.prewarm.start(host.clone());
        host.fire(UserActivityKind::Draft);
        settle(&h).await;
        assert_eq!(h.warms.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn ineligible_sessions_do_not_prewarm() {
        let cases: [fn(&Host); 5] = [
            |host| host.enabled.store(false, Ordering::SeqCst),
            |host| host.idle.store(false, Ordering::SeqCst),
            |host| host.pending.store(true, Ordering::SeqCst),
            |host| {
                host.model.lock().unwrap().as_mut().unwrap().cache_retention =
                    Some(CacheRetention::None)
            },
            |host| host.model.lock().unwrap().as_mut().unwrap().api = "anthropic-messages".into(),
        ];
        for configure in cases {
            let h = harness(Ok(supported()));
            let host = Host::new();
            configure(&host);
            h.prewarm.start(host.clone());
            host.fire(UserActivityKind::Draft);
            assert!(!h.prewarm.in_flight());
            assert_eq!(h.warms.load(Ordering::SeqCst), 0);
        }
    }

    #[tokio::test]
    async fn parked_session_waits_for_resume() {
        let h = harness(Ok(supported()));
        let host = Host::new();
        h.prewarm.start(host.clone());
        h.prewarm.on_session_parked();
        host.fire(UserActivityKind::Draft);
        assert_eq!(h.warms.load(Ordering::SeqCst), 0);
        h.prewarm.on_session_resumed(host.clone());
        host.fire(UserActivityKind::Draft);
        settle(&h).await;
        assert_eq!(h.warms.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn skipped_prefix_records_the_reason() {
        let h = harness(Ok(supported()));
        let host = Host::new();
        *host.prefix.lock().unwrap() = Some(Box::new(|| {
            Some(PromptCachePrefixOutcome::Skipped("not preview-safe".into()))
        }));
        h.prewarm.start(host.clone());
        host.fire(UserActivityKind::Draft);
        settle(&h).await;
        let entries = h.entries.lock().unwrap().clone();
        assert_eq!(entries[0].1["phase"], "skipped");
        assert_eq!(entries[0].1["reason"], "not preview-safe");
        assert_eq!(h.warms.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn warm_failure_records_the_error() {
        let h = harness(Err("boom".into()));
        let host = Host::new();
        h.prewarm.start(host.clone());
        host.fire(UserActivityKind::Draft);
        settle(&h).await;
        let entries = h.entries.lock().unwrap().clone();
        assert_eq!(
            entries[0].1,
            json!({"phase":"failed","provider":"openai","model":"gpt","error":"boom"})
        );
    }

    #[tokio::test]
    async fn unsupported_result_and_shutdown_record_nothing() {
        let h = harness(Ok(WarmPromptCacheResult::Unsupported));
        let host = Host::new();
        h.prewarm.start(host.clone());
        h.prewarm.on_session_shutdown();
        host.fire(UserActivityKind::Draft);
        assert_eq!(h.warms.load(Ordering::SeqCst), 0);
        assert!(h.entries.lock().unwrap().is_empty());
    }

    #[test]
    fn usage_cost_prefers_reported_cost() {
        let usage = WarmPromptCacheUsage {
            input: 1,
            output: 2,
            cache_read: 3,
            cache_write: 4,
            cost: Some(UsageCost {
                input: 0.1,
                output: 0.2,
                cache_read: 0.3,
                cache_write: 0.4,
                total: 99.0,
            }),
        };
        let usage = to_usage(&model(), &usage);
        assert_eq!(usage.total_tokens, 10);
        assert!((usage.cost.total - 1.0).abs() < 1e-12);
    }
}
