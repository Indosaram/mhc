pub mod timeout;
pub use timeout::*;

use maho_ext_api::{BeforeAgentStartEventResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionEvent};
use std::sync::Arc;

pub struct BashTimeout;
impl Extension for BashTimeout {
    fn register(&self, api: &mut ExtensionApi) {
        let defaults = resolve_bash_timeout_defaults(
            std::env::var("PI_BASH_DEFAULT_TIMEOUT_SECONDS").ok().as_deref(),
            std::env::var("PI_BASH_MAX_TIMEOUT_SECONDS").ok().as_deref(),
        );
        api.on(EventKind::ToolCall, Arc::new(move |event, _ctx| Box::pin(async move {
            if let ExtensionEvent::ToolCall(call) = event
                && call.tool_name == "bash" {
                call.input = apply_bash_timeout(&call.input, defaults);
            }
            Ok(EventResult::None)
        })));
        api.on(EventKind::BeforeAgentStart, Arc::new(move |event, ctx| Box::pin(async move {
            let ExtensionEvent::BeforeAgentStart(event) = event else { return Ok(EventResult::None) };
            if maho_ext_anthropic_bash::is_anthropic_bash_enabled() && ctx.model.as_ref().is_some_and(|model| model.api == "anthropic-messages") {
                return Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                    system_prompt: Some(format!("{}{}", event.system_prompt, build_bash_timeout_prompt(defaults, None))),
                    ..Default::default()
                }));
            }
            let window = std::env::var("PI_BASH_FOREGROUND_SECONDS").ok()
                .and_then(|value| value.parse::<f64>().ok()).filter(|value| value.is_finite() && *value > 0.0).unwrap_or(60.0);
            Ok(EventResult::BeforeAgentStart(BeforeAgentStartEventResult {
                system_prompt: Some(format!("{}{}", event.system_prompt, build_bash_timeout_prompt(defaults, Some(window)))),
                ..Default::default()
            }))
        })));
    }
}
