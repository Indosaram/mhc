//! Port of handoff-hint/index.ts.
use crate::brief::{HandoffBriefInput, build_handoff_brief};
use crate::estimate::{HandoffEstimate, estimate_handoff, hint_text};
use maho_ai::auth::types::CredentialType;
use maho_ext_api::{
    EventKind, EventResult, Extension, ExtensionApi, ExtensionContext, ExtensionEvent,
    ExtensionFailure, ExtensionMode, ExtensionWidgetAction, ExtensionWidgetOptions,
    InputEventResult, InputSource, Model, NewSessionOptions, NotificationType, SendMessageOptions,
    SessionEntry, SessionKind, ToolContent, WidgetContent, WidgetPlacement,
};
use maho_ext_goal::types::{Goal, GoalStatus};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use tokio::task::JoinHandle;

pub const HANDOFF_HINT_ENTRY_TYPE: &str = "handoff-hint";
pub const HANDOFF_BRIEF_MESSAGE_TYPE: &str = "handoff-brief";
pub const HANDOFF_HINT_EVENT: &str = "handoff_hint";
pub const HANDOFF_HINT_WIDGET_KEY: &str = "handoff-hint";
pub const HANDOFF_COMMAND: &str = "handoff";
pub const START_FRESH_LABEL: &str = "Start fresh with a handoff";
pub const KEEP_GOING_LABEL: &str = "Keep going";

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandoffHintSettings {
    pub enabled: bool,
    pub min_context_tokens: u64,
    pub min_saving_usd: f64,
}

pub const DEFAULT_HANDOFF_HINT_SETTINGS: HandoffHintSettings = HandoffHintSettings {
    enabled: true,
    min_context_tokens: 200_000,
    min_saving_usd: 1.0,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandoffHintAction {
    Shown,
    Accepted,
    Dismissed,
}

impl HandoffHintAction {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Shown => "shown",
            Self::Accepted => "accepted",
            Self::Dismissed => "dismissed",
        }
    }
}

pub type SettingsReader = Arc<dyn Fn() -> HandoffHintSettings + Send + Sync>;
pub type GoalReader = Arc<dyn Fn(&ExtensionContext) -> Option<Goal> + Send + Sync>;
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The extension. `settings` supplies `settings.handoffHint` (the host exposes no settings reader to
/// extensions yet), `read_goal` the session's goal, `now` the clock in Unix milliseconds.
pub struct HandoffHint {
    pub settings: SettingsReader,
    pub read_goal: GoalReader,
    pub now: Clock,
}

impl Default for HandoffHint {
    fn default() -> Self {
        Self {
            settings: Arc::new(|| DEFAULT_HANDOFF_HINT_SETTINGS),
            read_goal: Arc::new(|ctx| {
                let reference = maho_ext_goal::store_ref::context_goal_store_ref(ctx);
                maho_ext_goal::read_goal(&reference).ok().flatten()
            }),
            now: Arc::new(maho_ai::utils::diagnostics::now_ms),
        }
    }
}

pub fn prompt_tokens_of_first_reply(entries: &[SessionEntry]) -> Option<u64> {
    entries.iter().find_map(|entry| {
        if entry.data.get("type").and_then(Value::as_str) != Some("message") {
            return None;
        }
        let message = entry.data.get("message")?;
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        let usage = message.get("usage")?;
        let field = |name: &str| usage.get(name).and_then(Value::as_u64).unwrap_or(0);
        let tokens = match usage.get("contextUsage") {
            Some(context) => context.get("input").and_then(Value::as_u64).unwrap_or(0),
            None => field("input") + field("cacheRead") + field("cacheWrite"),
        };
        (tokens > 0).then_some(tokens)
    })
}

pub fn last_reply_at_ms(entries: &[SessionEntry]) -> Option<i64> {
    entries.iter().rev().find_map(|entry| {
        if entry.data.get("type").and_then(Value::as_str) != Some("message") {
            return None;
        }
        let message = entry.data.get("message")?;
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            return None;
        }
        message.get("timestamp").and_then(Value::as_i64)
    })
}

pub fn was_dismissed(entries: &[SessionEntry]) -> bool {
    entries.iter().any(|entry| {
        entry.data.get("type").and_then(Value::as_str) == Some("custom")
            && entry.data.get("customType").and_then(Value::as_str) == Some(HANDOFF_HINT_ENTRY_TYPE)
            && entry
                .data
                .get("data")
                .and_then(|data| data.get("phase"))
                .and_then(Value::as_str)
                == Some("dismissed")
    })
}

#[derive(Default)]
struct State {
    ctx: Option<ExtensionContext>,
    timer: Option<JoinHandle<()>>,
    last_request_at_ms: Option<i64>,
    human_started_turn: bool,
    dismissed: bool,
    shown_this_crossing: bool,
    visible: Option<HandoffEstimate>,
}

struct Shared {
    deps: HandoffHint,
    worker: bool,
    sender: ExtensionApi,
    state: Mutex<State>,
    ping: Mutex<Option<maho_ext_api::BusSubscription>>,
}

fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(PoisonError::into_inner)
}

fn is_subscription(ctx: &ExtensionContext, model: &Model) -> bool {
    matches!(
        ctx.model_registry
            .get_stored_credential_type(&model.provider),
        Ok(Some(CredentialType::OAuth))
    )
}

fn hint_event(
    action: HandoffHintAction,
    ctx: &ExtensionContext,
    estimate: &HandoffEstimate,
) -> Value {
    let model = ctx.model.as_ref();
    json!({
        "action": action.as_str(),
        "sessionId": ctx.session_manager.session_id(),
        "provider": model.map_or("", |model| model.provider.as_str()),
        "model": model.map_or("", |model| model.id.as_str()),
        "contextTokens": estimate.context_tokens,
        "nextReplyUsd": estimate.next_reply_usd,
        "freshUsd": estimate.fresh_usd,
        "savingUsd": estimate.saving_usd,
        "subscription": model.is_some_and(|model| is_subscription(ctx, model)),
    })
}

impl Shared {
    fn emit(&self, action: HandoffHintAction, ctx: &ExtensionContext, estimate: &HandoffEstimate) {
        self.sender
            .events
            .emit(HANDOFF_HINT_EVENT, &hint_event(action, ctx, estimate));
    }

    fn render(&self, ctx: &ExtensionContext, estimate: &HandoffEstimate) {
        let subscription = ctx
            .model
            .as_ref()
            .is_some_and(|model| is_subscription(ctx, model));
        lock(&self.state).visible = Some(estimate.clone());
        ctx.ui.set_widget(
            HANDOFF_HINT_WIDGET_KEY,
            Some(WidgetContent::Lines(vec![hint_text(
                estimate,
                subscription,
            )])),
            ExtensionWidgetOptions {
                placement: WidgetPlacement::AboveEditor,
                actions: Some(vec![
                    ExtensionWidgetAction {
                        label: START_FRESH_LABEL.to_owned(),
                        command: format!("/{HANDOFF_COMMAND}"),
                    },
                    ExtensionWidgetAction {
                        label: KEEP_GOING_LABEL.to_owned(),
                        command: format!("/{HANDOFF_COMMAND} keep"),
                    },
                ]),
            },
        );
    }

    fn disarm(&self) {
        if let Some(timer) = lock(&self.state).timer.take() {
            timer.abort();
        }
    }

    fn hide(&self, ctx: Option<&ExtensionContext>) {
        if lock(&self.state).visible.take().is_none() {
            return;
        }
        if let Some(ctx) = ctx {
            ctx.ui.set_widget(
                HANDOFF_HINT_WIDGET_KEY,
                None,
                ExtensionWidgetOptions::default(),
            );
        }
    }

    fn eligible_estimate(
        &self,
        state: &mut State,
        ctx: &ExtensionContext,
    ) -> Option<HandoffEstimate> {
        let settings = (self.deps.settings)();
        let model = ctx.model.as_ref()?;
        if !settings.enabled || state.dismissed || self.worker || !state.human_started_turn {
            return None;
        }
        if !matches!(ctx.mode, ExtensionMode::Tui | ExtensionMode::Rpc) {
            return None;
        }
        if !ctx.has_ui || !ctx.is_idle() || ctx.has_pending_messages().ok()? || ctx.is_compacting()
        {
            return None;
        }
        let usage = ctx.get_context_usage().ok()??;
        let tokens = usage.tokens?;
        if tokens < settings.min_context_tokens {
            state.shown_this_crossing = false;
            return None;
        }
        if state.shown_this_crossing {
            return None;
        }
        if usage.context_window > 0
            && tokens as f64
                >= usage.context_window as f64
                    * maho_ext_compaction::policy::base_threshold_ratio_for_window(
                        usage.context_window as f64,
                    )
        {
            return None;
        }
        if (self.deps.read_goal)(ctx).is_some_and(|goal| goal.status == GoalStatus::Active) {
            return None;
        }
        let fresh =
            prompt_tokens_of_first_reply(&ctx.session_manager.get_branch()).unwrap_or(tokens);
        let estimate = estimate_handoff(model, tokens, fresh, (self.deps.now)())?;
        (estimate.saving_usd >= settings.min_saving_usd).then_some(estimate)
    }

    fn show_if_due(&self) {
        let (ctx, estimate) = {
            let mut state = lock(&self.state);
            state.timer = None;
            let Some(ctx) = state.ctx.clone() else {
                return;
            };
            if state.visible.is_some() {
                return;
            }
            let Some(estimate) = self.eligible_estimate(&mut state, &ctx) else {
                return;
            };
            if !ctx.is_idle() {
                return;
            }
            state.shown_this_crossing = true;
            (ctx, estimate)
        };
        self.render(&ctx, &estimate);
        self.emit(HandoffHintAction::Shown, &ctx, &estimate);
    }
}

fn arm(shared: &Arc<Shared>) {
    shared.disarm();
    let delay_ms = {
        let state = lock(&shared.state);
        let (Some(ctx), Some(last)) = (state.ctx.as_ref(), state.last_request_at_ms) else {
            return;
        };
        let Some(model) = ctx.model.as_ref() else {
            return;
        };
        if state.dismissed || shared.worker || !(shared.deps.settings)().enabled {
            return;
        }
        let Some(ttl_seconds) =
            maho_ai::utils::prompt_cache_ttl::resolve_prompt_cache_ttl_seconds(model, None)
        else {
            return;
        };
        (last + ttl_seconds as i64 * 1000 - (shared.deps.now)()).max(0) as u64
    };
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let task = Arc::clone(shared);
    let handle = runtime.spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
        task.show_if_due();
    });
    lock(&shared.state).timer = Some(handle);
}

fn unknown_argument(argument: &str) -> String {
    format!(
        "Unknown /{HANDOFF_COMMAND} argument \"{argument}\". Use /{HANDOFF_COMMAND} to start fresh or /{HANDOFF_COMMAND} keep to keep going."
    )
}

async fn start_fresh(
    shared: &Arc<Shared>,
    ctx: &maho_ext_api::ExtensionCommandContext,
) -> Result<(), ExtensionFailure> {
    let estimate = lock(&shared.state).visible.clone();
    let usage = ctx.get_context_usage()?;
    let goal = (shared.deps.read_goal)(ctx);
    let model_label = ctx
        .model
        .as_ref()
        .map(|model| format!("{}/{}", model.provider, model.id));
    let session_file = ctx
        .session_manager
        .session_file()
        .map(|path| path.to_string_lossy().into_owned());
    let entries = ctx.session_manager.get_branch();
    let cwd = ctx.cwd.to_string_lossy().into_owned();
    let brief = build_handoff_brief(&HandoffBriefInput {
        entries: &entries,
        goal: goal.as_ref(),
        cwd: &cwd,
        model_label: model_label.as_deref(),
        previous_session_file: session_file.as_deref(),
        context_tokens: usage
            .and_then(|usage| usage.tokens)
            .or_else(|| estimate.as_ref().map(|estimate| estimate.context_tokens))
            .unwrap_or(0),
    });
    let accepted = estimate
        .as_ref()
        .map(|estimate| hint_event(HandoffHintAction::Accepted, ctx, estimate));
    let session_id = ctx.session_manager.session_id().to_owned();
    shared.hide(Some(&ctx.context));
    let brief = Arc::new(brief);
    let result = ctx
        .new_session(NewSessionOptions {
            parent_session: session_file,
            setup: None,
            with_session: Some(Arc::new(move |next| {
                let brief = Arc::clone(&brief);
                Box::pin(async move {
                    next.send_message(
                        maho_ext_api::CustomMessage {
                            custom_type: HANDOFF_BRIEF_MESSAGE_TYPE.into(),
                            content: vec![ToolContent::text(brief.as_str())],
                            display: true,
                            details: None,
                        },
                        SendMessageOptions {
                            trigger_turn: false,
                            deliver_as: None,
                        },
                    )
                    .await
                })
            })),
        })
        .await?;
    if result.cancelled {
        if let Some(estimate) = estimate
            && ctx.session_manager.session_id() == session_id
        {
            shared.render(ctx, &estimate);
        }
        return Ok(());
    }
    if let Some(accepted) = accepted {
        shared.sender.events.emit(HANDOFF_HINT_EVENT, &accepted);
    }
    Ok(())
}

impl Extension for HandoffHint {
    fn register(&self, api: &mut ExtensionApi) {
        let shared = Arc::new(Shared {
            deps: HandoffHint {
                settings: Arc::clone(&self.settings),
                read_goal: Arc::clone(&self.read_goal),
                now: Arc::clone(&self.now),
            },
            worker: api.profile.session_kind == SessionKind::Worker,
            sender: ExtensionApi::new(
                api.registered.clone(),
                api.profile.clone(),
                api.events.clone(),
                api.runtime.clone(),
            ),
            state: Mutex::new(State::default()),
            ping: Mutex::new(None),
        });

        let command_shared = Arc::clone(&shared);
        api.register_command_with_context(
            HANDOFF_COMMAND,
            Some(
                "Start a fresh session seeded with a short handoff brief (`/handoff keep` hides the hint)"
                    .into(),
            ),
            Some("[keep]".into()),
            Arc::new(move |args, ctx| {
                let shared = Arc::clone(&command_shared);
                Box::pin(async move {
                    let argument = args.trim();
                    if !argument.is_empty() && argument != "keep" {
                        ctx.ui
                            .notify(&unknown_argument(argument), NotificationType::Warning);
                        return Ok(());
                    }
                    if argument == "keep" {
                        let estimate = {
                            let mut state = lock(&shared.state);
                            state.dismissed = true;
                            state.visible.clone()
                        };
                        shared.disarm();
                        shared.hide(Some(&ctx.context));
                        shared
                            .sender
                            .append_entry(HANDOFF_HINT_ENTRY_TYPE, Some(json!({"phase": "dismissed"})))?;
                        if let Some(estimate) = estimate {
                            shared.emit(HandoffHintAction::Dismissed, ctx, &estimate);
                        }
                        return Ok(());
                    }
                    start_fresh(&shared, ctx).await
                })
            }),
        );

        for kind in [
            EventKind::SessionStart,
            EventKind::Input,
            EventKind::AgentStart,
            EventKind::AgentEnd,
            EventKind::AgentSettled,
            EventKind::SessionCompact,
            EventKind::ModelSelect,
            EventKind::SessionShutdown,
        ] {
            let shared = Arc::clone(&shared);
            api.on(
                kind,
                Arc::new(move |event, ctx| {
                    let shared = Arc::clone(&shared);
                    let mut result = EventResult::None;
                    match event {
                        ExtensionEvent::SessionStart(_) => {
                            shared.disarm();
                            let branch = ctx.session_manager.get_branch();
                            {
                                let mut state = lock(&shared.state);
                                state.ctx = Some(ctx.clone());
                                state.visible = None;
                                state.shown_this_crossing = false;
                                state.dismissed = was_dismissed(&branch);
                                // A session opened with history belongs to whoever opened it, and its
                                // cache aged from the last reply: arm from there.
                                state.last_request_at_ms = last_reply_at_ms(&branch);
                                state.human_started_turn = state.last_request_at_ms.is_some();
                            }
                            arm(&shared);
                        }
                        ExtensionEvent::Input(input) => {
                            lock(&shared.state).human_started_turn =
                                input.source != InputSource::Extension;
                            shared.disarm();
                            let current = lock(&shared.state).ctx.clone();
                            shared.hide(current.as_ref());
                            result = EventResult::Input(InputEventResult::Continue);
                        }
                        ExtensionEvent::AgentStart => {
                            shared.disarm();
                            let current = lock(&shared.state).ctx.clone();
                            shared.hide(current.as_ref());
                        }
                        ExtensionEvent::AgentEnd { .. } => {
                            let mut state = lock(&shared.state);
                            state.ctx = Some(ctx.clone());
                            state.last_request_at_ms = Some((shared.deps.now)());
                        }
                        ExtensionEvent::AgentSettled => {
                            lock(&shared.state).ctx = Some(ctx.clone());
                            arm(&shared);
                        }
                        ExtensionEvent::SessionCompact(_) => {
                            let current = lock(&shared.state).ctx.clone();
                            shared.hide(current.as_ref());
                            arm(&shared);
                        }
                        ExtensionEvent::ModelSelect(selected) => {
                            let mut next = ctx.clone();
                            next.model = Some(selected.model.clone());
                            lock(&shared.state).ctx = Some(next);
                            arm(&shared);
                        }
                        ExtensionEvent::SessionShutdown(_) => {
                            shared.disarm();
                            lock(&shared.state).ctx = None;
                        }
                        _ => {}
                    }
                    Box::pin(async move { Ok(result) })
                }),
            );
        }

        let ping_shared = Arc::downgrade(&shared);
        let subscription = api.events.on(
            maho_ext_cache_keepalive::CACHE_WARM_PING_EVENT,
            Arc::new(move |_| {
                let Some(shared) = ping_shared.upgrade() else {
                    return;
                };
                let armed = {
                    let mut state = lock(&shared.state);
                    state.last_request_at_ms = Some((shared.deps.now)());
                    state.timer.is_some()
                };
                if armed {
                    arm(&shared);
                }
            }),
        );
        *shared.ping.lock().unwrap_or_else(PoisonError::into_inner) = Some(subscription);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(data: Value) -> SessionEntry {
        SessionEntry {
            id: "e".into(),
            parent_id: None,
            timestamp: String::new(),
            kind: String::new(),
            data,
        }
    }

    fn assistant(timestamp: i64, input: u64, cache_read: u64, cache_write: u64) -> SessionEntry {
        entry(json!({"type": "message", "message": {
            "role": "assistant", "timestamp": timestamp,
            "usage": {"input": input, "cacheRead": cache_read, "cacheWrite": cache_write}
        }}))
    }

    #[test]
    fn first_reply_prompt_tokens_skip_empty_usage() {
        let entries = [
            assistant(1, 0, 0, 0),
            assistant(2, 10, 20, 30),
            assistant(3, 99, 0, 0),
        ];
        assert_eq!(prompt_tokens_of_first_reply(&entries), Some(60));
        assert_eq!(prompt_tokens_of_first_reply(&[]), None);
    }

    #[test]
    fn first_reply_prompt_tokens_prefer_context_usage_over_billed_tokens() {
        let mut reply = assistant(1, 10, 20, 30);
        reply.data["message"]["usage"]["contextUsage"] = json!({"input": 90});
        assert_eq!(prompt_tokens_of_first_reply(&[reply]), Some(90));
    }

    #[test]
    fn first_reply_prompt_tokens_skip_explicitly_unknown_context_usage() {
        let mut reply = assistant(1, 10, 20, 30);
        reply.data["message"]["usage"]["contextUsage"] = Value::Null;
        assert_eq!(prompt_tokens_of_first_reply(&[reply, assistant(2, 40, 0, 0)]), Some(40));
    }

    #[test]
    fn last_reply_timestamp_is_the_newest_assistant_message() {
        let entries = [
            assistant(5, 1, 0, 0),
            entry(json!({"type": "message", "message": {"role": "user", "timestamp": 9}})),
            assistant(7, 1, 0, 0),
            entry(json!({"type": "message", "message": {"role": "user", "timestamp": 11}})),
        ];
        assert_eq!(last_reply_at_ms(&entries), Some(7));
    }

    #[test]
    fn dismissal_is_read_from_custom_entries() {
        let dismissed = entry(json!({
            "type": "custom", "customType": HANDOFF_HINT_ENTRY_TYPE, "data": {"phase": "dismissed"}
        }));
        let other =
            entry(json!({"type": "custom", "customType": "x", "data": {"phase": "dismissed"}}));
        assert!(was_dismissed(&[other.clone(), dismissed]));
        assert!(!was_dismissed(&[other]));
    }

    #[test]
    fn unknown_argument_message_matches_upstream() {
        assert_eq!(
            unknown_argument("later"),
            "Unknown /handoff argument \"later\". Use /handoff to start fresh or /handoff keep to keep going."
        );
    }
}
