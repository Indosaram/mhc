//! Event wiring for arming and request-local directive selection.

use crate::engine::{Engine, InputRequest, TodoResultView};
use crate::variant::Variant;
use maho_ext_api::{
    BeforeAgentStartEventResult, ContextEventSource, EventKind, EventResult, Extension,
    ExtensionApi, ExtensionContext, ExtensionEvent, ExtensionHandler, ExtensionHandlerOptions,
    FlagType, FlagValue, InputEventResult, SessionCompactEvent, ToolContent, ToolResultEventResult,
};
use std::path::Path;
use std::sync::Arc;

/// Runtime flag that turns the whole engine off.
pub const DISABLE_FLAG: &str = "ultrawork-disabled";

type LoadKeywordPolicy = dyn Fn(&Path) -> bool + Send + Sync;

#[derive(Clone)]
struct KeywordPolicy {
    load: Arc<LoadKeywordPolicy>,
    legacy_child: bool,
}

#[derive(Default, Clone)]
pub struct UltraworkExtension {
    engine: Engine,
    keyword_policy: Option<KeywordPolicy>,
}

impl UltraworkExtension {
    pub fn new(engine: Engine) -> Self {
        Self {
            engine,
            keyword_policy: None,
        }
    }

    /// Loads activation policy from the registration directory and takes legacy child detection
    /// from the same host environment as the loader.
    #[must_use]
    pub fn with_keyword_policy(
        mut self,
        load: impl Fn(&Path) -> bool + Send + Sync + 'static,
        legacy_child: bool,
    ) -> Self {
        self.keyword_policy = Some(KeywordPolicy {
            load: Arc::new(load),
            legacy_child,
        });
        self
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

fn session_of(ctx: &ExtensionContext) -> String {
    ctx.session_manager.session_id().to_owned()
}

fn variant_of(ctx: &ExtensionContext) -> Variant {
    Variant::for_model(ctx.model.as_ref().map(|model| model.id.as_str()))
}

fn is_delegated(profile_role: Option<&str>, legacy_child: Option<bool>) -> bool {
    profile_role.is_some_and(|role| !role.is_empty())
        || legacy_child.unwrap_or_else(|| {
            std::env::var_os("SENPI_TASK_MEMBER").is_some()
                || std::env::var_os("OMO_SENPI_TASK_RPC_CHILD").is_some_and(|value| value == "1")
        })
}

impl Extension for UltraworkExtension {
    fn register(&self, api: &mut ExtensionApi) {
        api.register_flag(
            DISABLE_FLAG,
            FlagType::Boolean {
                default: Some(false),
            },
            Some("Turn the ultrawork keyword engine off".into()),
        );
        let runtime = api.runtime.clone();
        let disabled = Arc::new(move || {
            matches!(
                runtime.get_flag(DISABLE_FLAG),
                Some(FlagValue::Boolean(true))
            )
        });

        let (engine, off) = (self.engine.clone(), Arc::clone(&disabled));
        let keywords_allowed = self
            .keyword_policy
            .as_ref()
            .is_none_or(|policy| (policy.load)(&api.cwd));
        let legacy_child = self
            .keyword_policy
            .as_ref()
            .map(|policy| policy.legacy_child);
        let session_role = api.profile.session_context.get("role").cloned();
        let projection_role = session_role.clone();
        let on_input: ExtensionHandler = Arc::new(move |event, ctx| {
            let (engine, off) = (engine.clone(), Arc::clone(&off));
            let delegated = is_delegated(session_role.as_deref(), legacy_child);
            Box::pin(async move {
                let input = match event {
                    ExtensionEvent::Input(typed) => typed,
                    _ => return Ok(EventResult::None),
                };
                if off() || !keywords_allowed {
                    return Ok(EventResult::Input(InputEventResult::Continue));
                }
                let session = session_of(ctx);
                let outcome = engine.on_input_for_variant(
                    InputRequest {
                        session: &session,
                        text: &input.text,
                        source: input.source,
                        streaming: input.streaming_behavior.is_some(),
                    },
                    false,
                    delegated.then_some("child"),
                    variant_of(ctx),
                );
                Ok(EventResult::Input(match outcome.transformed_text {
                    Some(text) => InputEventResult::Transform {
                        text,
                        images: input.images.clone(),
                    },
                    None => InputEventResult::Continue,
                }))
            })
        });
        api.on(EventKind::Input, on_input);

        let engine = self.engine.clone();
        let on_turn: ExtensionHandler = Arc::new(move |event, ctx| {
            let engine = engine.clone();
            Box::pin(async move {
                let start = match event {
                    ExtensionEvent::BeforeAgentStart(typed) => typed,
                    _ => return Ok(EventResult::None),
                };
                let message = engine.on_turn_start_for_variant(
                    &session_of(ctx),
                    start.preview,
                    variant_of(ctx),
                );
                Ok(match message {
                    Some(message) => EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                        message: Some(message),
                        ..Default::default()
                    }),
                    None => EventResult::None,
                })
            })
        });
        api.on_with_options(
            EventKind::BeforeAgentStart,
            on_turn,
            ExtensionHandlerOptions {
                preview_safe: true,
                mutates_messages: None,
                ..Default::default()
            },
        );

        let (engine, off) = (self.engine.clone(), Arc::clone(&disabled));
        let on_context: ExtensionHandler = Arc::new(move |event, ctx| {
            let (engine, off) = (engine.clone(), Arc::clone(&off));
            let role = projection_role.as_deref();
            let is_live_owner = matches!(
                event,
                ExtensionEvent::Context {
                    source: ContextEventSource::Agent,
                    ..
                }
            ) && !is_delegated(role, legacy_child);
            Box::pin(async move {
                if let ExtensionEvent::Context { messages, .. } = event
                    && !off()
                    && variant_of(ctx).project(messages)
                    && is_live_owner
                    && !ctx.is_compacting()
                {
                    engine
                        .sessions()
                        .restore_arming_from_context(ctx.session_manager.session_id());
                }
                Ok(EventResult::None)
            })
        });
        api.on(EventKind::Context, on_context);

        let engine = self.engine.clone();
        let on_compact: ExtensionHandler = Arc::new(move |event, ctx| {
            let engine = engine.clone();
            Box::pin(async move {
                if matches!(
                    event,
                    ExtensionEvent::SessionCompact(SessionCompactEvent::Accepted { .. })
                ) {
                    engine.on_compaction(&session_of(ctx));
                }
                Ok(EventResult::None)
            })
        });
        api.on(EventKind::SessionCompact, on_compact);

        let (engine, off) = (self.engine.clone(), disabled);
        let on_result: ExtensionHandler = Arc::new(move |event, ctx| {
            let (engine, off) = (engine.clone(), Arc::clone(&off));
            Box::pin(async move {
                let result = match event {
                    ExtensionEvent::ToolResult(typed) => typed,
                    _ => return Ok(EventResult::None),
                };
                let session = session_of(ctx);
                let view = TodoResultView {
                    session: &session,
                    tool_name: &result.tool_name,
                    is_error: result.is_error,
                    details: result.details.as_ref(),
                };
                Ok(match engine.on_tool_result(view, off()) {
                    Some(advisory) => {
                        let mut content = result.content.clone();
                        content.push(ToolContent::text(advisory));
                        EventResult::ToolResult(ToolResultEventResult {
                            content: Some(content),
                            ..Default::default()
                        })
                    }
                    None => EventResult::None,
                })
            })
        });
        api.on(EventKind::ToolResult, on_result);
    }
}

/// Applies the keyword lists from a validated `omo.jsonc` view.
pub fn keyword_activation_allowed(config: &serde_json::Value) -> bool {
    let contains_trigger = |values: &[serde_json::Value]| {
        values
            .iter()
            .any(|value| value.as_str() == Some("ultrawork"))
    };
    let settings = &config["keyword_detector"];
    !settings["disabled_keywords"]
        .as_array()
        .is_some_and(|values| contains_trigger(values))
        && settings["enabled_expansions"]
            .as_array()
            .is_none_or(|values| contains_trigger(values))
}
