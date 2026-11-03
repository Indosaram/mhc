//! Event wiring. No context-transform handler is registered: the engine never touches history.

use crate::engine::{Engine, InputRequest, TodoResultView};
use maho_ext_api::{
    BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext,
    ExtensionEvent, ExtensionHandlerOptions, FlagType, FlagValue, InputEventResult,
    SessionCompactEvent, ToolContent, ToolResultEventResult,
};
use std::sync::Arc;

/// Runtime flag that turns the whole engine off.
pub const DISABLE_FLAG: &str = "ultrawork-disabled";

#[derive(Clone, Default)]
pub struct UltraworkExtension {
    engine: Engine,
}

impl UltraworkExtension {
    pub fn new(engine: Engine) -> Self {
        Self { engine }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }
}

fn session_of(ctx: &ExtensionContext) -> String {
    ctx.session_manager.session_id().to_owned()
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
        api.on(
            EventKind::Input,
            Arc::new(move |event, ctx| {
                let (engine, off) = (engine.clone(), Arc::clone(&off));
                Box::pin(async move {
                    let ExtensionEvent::Input(input) = event else {
                        return Ok(EventResult::None);
                    };
                    if off() {
                        return Ok(EventResult::Input(InputEventResult::Continue));
                    }
                    let session = session_of(ctx);
                    let outcome = engine.on_input(
                        InputRequest {
                            session: &session,
                            text: &input.text,
                            source: input.source,
                            streaming: input.streaming_behavior.is_some(),
                        },
                        false,
                    );
                    Ok(EventResult::Input(match outcome.transformed_text {
                        Some(text) => InputEventResult::Transform {
                            text,
                            images: input.images.clone(),
                        },
                        None => InputEventResult::Continue,
                    }))
                })
            }),
        );

        let engine = self.engine.clone();
        api.on_with_options(
            EventKind::BeforeAgentStart,
            Arc::new(move |event, ctx| {
                let engine = engine.clone();
                Box::pin(async move {
                    let ExtensionEvent::BeforeAgentStart(start) = event else {
                        return Ok(EventResult::None);
                    };
                    Ok(
                        match engine.on_turn_start(&session_of(ctx), start.preview) {
                            Some(message) => {
                                EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                                    message: Some(message),
                                    ..Default::default()
                                })
                            }
                            None => EventResult::None,
                        },
                    )
                })
            }),
            ExtensionHandlerOptions {
                preview_safe: true,
                mutates_messages: None,
            },
        );

        let engine = self.engine.clone();
        api.on(
            EventKind::SessionCompact,
            Arc::new(move |event, ctx| {
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
            }),
        );

        let (engine, off) = (self.engine.clone(), disabled);
        api.on(
            EventKind::ToolResult,
            Arc::new(move |event, ctx| {
                let (engine, off) = (engine.clone(), Arc::clone(&off));
                Box::pin(async move {
                    let ExtensionEvent::ToolResult(result) = event else {
                        return Ok(EventResult::None);
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
            }),
        );
    }
}
