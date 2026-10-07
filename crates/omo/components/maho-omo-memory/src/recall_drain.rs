//! Delivery half of the kibitzer recall channel (latest `recall-drain.ts`).
//!
//! `before_agent_start` drains the pending file the sidecar's delivery wrote, unions it with the
//! in-memory queue, dedupes by path, injects ONE hidden `omo-kibitzer:recall` message, marks the
//! ledger and appends the visible trace. Fail-open: an unreadable ledger or a failed trace never
//! suppresses a nudge the sidecar already paid for, and a memory worker child never receives one.

use std::sync::Arc;

use memory_core::recall::{RecallLedger, RecallNudge};
use serde_json::Value;

use crate::context::MemoryIdentityContext;
use crate::kibitzer_notice::{
    KibitzerGateRecord, KibitzerNudgedRecord, NUDGED_ENTRY_TYPE,
    kibitzer_gate_spec, kibitzer_nudged_spec,
};
use crate::recall_consumer::{build_recall_injection, mark_recall_surfaced, recall_drain_union};
use crate::recall_notice::{MemoryRecallRecord, recall_notice_spec};
use crate::recall_session_read::RECALL_CUSTOM_TYPE;
use crate::worker::completion_renderers::ResolveEntryTheme;
use crate::worker::entry_renderers::{NoticeComponent, NoticeSpec};

/// The pending handoff delivery writes and this turn drains; `take` is read-and-delete.
pub trait PendingNudgesPort: Send + Sync {
    fn take(&self, session_id: &str) -> Vec<RecallNudge>;
}

/// The injected environment lookup (`options.env`).
pub type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;
/// The pending-nudge port for one identity (`options.pendingFor`).
pub type DrainPendingFor = Arc<dyn Fn(&MemoryIdentityContext) -> Arc<dyn PendingNudgesPort> + Send + Sync>;
/// The in-memory queued-nudge drain for one session (`options.drainQueued`).
pub type DrainQueued = Arc<dyn Fn(&str, &MemoryIdentityContext) -> Vec<RecallNudge> + Send + Sync>;

/// `createRecallDrain` options.
pub struct RecallDrainOptions {
    pub resolve_context: crate::prompt::PromptContextResolver,
    pub resolve_settings: Arc<dyn Fn() -> Value + Send + Sync>,
    pub env: EnvLookup,
    pub ledger_for: Arc<dyn Fn(&MemoryIdentityContext) -> RecallLedger + Send + Sync>,
    pub pending_for: DrainPendingFor,
    pub drain_queued: Option<DrainQueued>,
    pub warn: Arc<dyn Fn(&str) + Send + Sync>,
}

/// The delivery wiring.
pub struct RecallDrain {
    options: RecallDrainOptions,
}

impl RecallDrain {
    /// Registers the recall/nudged/gate/unavailable renderers (and their legacy read aliases) plus
    /// the `before_agent_start` delivery handler.
    pub fn register(&self, api: &mut maho_ext_api::ExtensionApi, theme: ResolveEntryTheme) {
        crate::recall_notice::register_recall_notice_renderer(api, theme.clone());
        crate::kibitzer_notice::register_kibitzer_notice_renderers(api, theme.clone());
        // Legacy read aliases keep stored sessions renderable; new entries use Kibitzer only.
        register_spec_alias(api, "omo-memorian:recall", theme.clone(), |data| {
            serde_json::from_value::<MemoryRecallRecord>(data).ok().and_then(|record| recall_notice_spec(&record))
        });
        register_spec_alias(api, "omo-memorian:nudged", theme.clone(), |data| {
            serde_json::from_value::<KibitzerNudgedRecord>(data).ok().and_then(|record| kibitzer_nudged_spec(&record))
        });
        register_spec_alias(api, "omo-memorian:gate", theme, |data| {
            serde_json::from_value::<KibitzerGateRecord>(data).ok().and_then(|record| kibitzer_gate_spec(&record))
        });

        let options = Arc::new(RecallDrainOptionsView {
            resolve_context: self.options.resolve_context.clone(),
            resolve_settings: self.options.resolve_settings.clone(),
            env: self.options.env.clone(),
            ledger_for: self.options.ledger_for.clone(),
            pending_for: self.options.pending_for.clone(),
            drain_queued: self.options.drain_queued.clone(),
            warn: self.options.warn.clone(),
        });
        let actions = Arc::new(maho_ext_api::ExtensionApi::new(
            maho_ext_api::LoadedExtension::new("memory-recall-drain", api.cwd.clone(), Default::default()),
            api.profile.clone(),
            api.events.clone(),
            api.runtime.clone(),
        ));
        api.on(
            maho_ext_api::EventKind::BeforeAgentStart,
            Arc::new(move |event, context| {
                let options = options.clone();
                let actions = actions.clone();
                Box::pin(async move {
                    if !matches!(event, maho_ext_api::ExtensionEvent::BeforeAgentStart(_)) {
                        return Ok(maho_ext_api::EventResult::None);
                    }
                    let session_id = context.session_manager.session_id();
                    if session_id.is_empty() {
                        return Ok(maho_ext_api::EventResult::None);
                    }
                    let Some(injection) = deliver(&options, session_id) else {
                        return Ok(maho_ext_api::EventResult::None);
                    };
                    if let Ok(value) = serde_json::to_value(&injection.record) {
                        let _ = actions.append_entry(NUDGED_ENTRY_TYPE, Some(value));
                    }
                    Ok(maho_ext_api::EventResult::BeforeAgentStart(
                        maho_ext_api::BeforeAgentStartEventResult {
                            message: Some(maho_ext_api::CustomMessage {
                                custom_type: RECALL_CUSTOM_TYPE.into(),
                                content: vec![maho_tools::definition::ToolContent::text(injection.message_content)],
                                display: false,
                                details: None,
                            }),
                            system_prompt: None,
                        },
                    ))
                })
            }),
        );
    }
}

/// `createRecallDrain`.
pub fn create_recall_drain(options: RecallDrainOptions) -> RecallDrain {
    RecallDrain { options }
}

struct RecallDrainOptionsView {
    resolve_context: crate::prompt::PromptContextResolver,
    resolve_settings: Arc<dyn Fn() -> Value + Send + Sync>,
    env: EnvLookup,
    ledger_for: Arc<dyn Fn(&MemoryIdentityContext) -> RecallLedger + Send + Sync>,
    pending_for: DrainPendingFor,
    drain_queued: Option<DrainQueued>,
    warn: Arc<dyn Fn(&str) + Send + Sync>,
}

struct Delivery {
    message_content: String,
    record: KibitzerNudgedRecord,
}

/// The pure delivery decision: drains, dedupes, builds the injection and marks the ledger.
fn deliver(options: &RecallDrainOptionsView, session_id: &str) -> Option<Delivery> {
    if child_sentinel(&options.env) {
        return None;
    }
    let context = (options.resolve_context)(session_id)?;
    let settings = (options.resolve_settings)();
    let recall = crate::reflection_settings::resolve_agent_recall_settings(Some(&settings), &context.identity);
    if recall.as_ref().ok().and_then(|settings| settings.get("enabled")).and_then(Value::as_bool) == Some(false) {
        return None;
    }

    let from_file = (options.pending_for)(&context).take(session_id);
    let from_queue = match &options.drain_queued {
        Some(drain) => drain(session_id, &context),
        None => Vec::new(),
    };
    let nudges = recall_drain_union(&from_queue, &from_file);
    let injection = build_recall_injection(&nudges, session_id)?;
    if let Err(error) = mark_recall_surfaced(&(options.ledger_for)(&context), session_id, &nudges) {
        (options.warn)(&format!("memory recall ledger mark skipped: {error}"));
    }
    Some(Delivery {
        message_content: injection.message_content,
        record: KibitzerNudgedRecord {
            version: 1,
            nudges: nudges
                .iter()
                .map(|nudge| crate::kibitzer_notice::KibitzerNudge { path: nudge.path.clone(), hint: nudge.hint.clone() })
                .collect(),
            via: Some("prompt".into()),
        },
    })
}

/// A memory worker child must never receive recall hints (reflection/facts sentinels).
fn child_sentinel(env: &EnvLookup) -> bool {
    ["SENPI_MEMORY_REFLECTION", "SENPI_MEMORY_FACTS"]
        .iter()
        .any(|name| env(name).as_deref() == Some("1"))
}

fn register_spec_alias(
    api: &mut maho_ext_api::ExtensionApi,
    entry_type: &str,
    theme: ResolveEntryTheme,
    spec_of: impl Fn(Value) -> Option<NoticeSpec> + Send + Sync + 'static,
) {
    api.register_entry_renderer(
        entry_type,
        Arc::new(move |entry, options, native_theme| {
            let data = entry.data.get("data")?.clone();
            let spec = spec_of(data)?;
            Some(Box::new(NoticeComponent { spec, expanded: options.expanded, theme: theme(native_theme) }) as Box<dyn maho_ext_api::Component>)
        }),
        Default::default(),
    );
}

#[cfg(test)]
#[path = "recall_drain_tests.rs"]
mod tests;
