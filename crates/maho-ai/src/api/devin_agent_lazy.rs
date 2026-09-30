//! Port of senpi packages/ai/src/api/devin-agent.lazy.ts.

use std::sync::{Arc, LazyLock, Mutex};

use crate::api::lazy::{lazy_api, LazyApiCapabilities};
use crate::types::ProviderStreams;

/// The statically bundled concrete module. senpi's lazy wrapper imports `./devin-agent.ts` through a
/// variable specifier so bundlers cannot follow the Node-only Connect transport into a browser
/// build; Rust has no such boundary, so the module is held here once and handed out on every load.
static CONCRETE_MODULE: LazyLock<Arc<dyn ProviderStreams>> = LazyLock::new(|| {
    struct DevinAgentModule;

    impl ProviderStreams for DevinAgentModule {
        fn stream(
            &self,
            model: &crate::types::Model,
            context: &crate::types::Context,
            options: Option<crate::types::StreamOptions>,
        ) -> crate::types::AssistantMessageEventStream {
            crate::api::devin_agent::stream(model, context, options)
        }

        fn stream_simple(
            &self,
            model: &crate::types::Model,
            context: &crate::types::Context,
            options: Option<crate::types::SimpleStreamOptions>,
        ) -> crate::types::AssistantMessageEventStream {
            crate::api::devin_agent::stream_simple(model, context, options)
        }
    }

    Arc::new(DevinAgentModule)
});

static MODULE_OVERRIDE: Mutex<Option<Arc<dyn ProviderStreams>>> = Mutex::new(None);

/// `setDevinAgentProviderModule`: installs the statically bundled implementation in a standalone
/// Bun isolate (a host-provided module in the Rust port).
pub fn set_devin_agent_provider_module(module: Arc<dyn ProviderStreams>) {
    *MODULE_OVERRIDE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(module);
}

/// `loadDevinAgentModule`: the newest override, else the concrete module.
pub fn load_devin_agent_module() -> Arc<dyn ProviderStreams> {
    MODULE_OVERRIDE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .unwrap_or_else(|| CONCRETE_MODULE.clone())
}

/// `devinAgentApi`.
pub fn devin_agent_api() -> Arc<dyn ProviderStreams> {
    lazy_api(Arc::new(|| Ok(load_devin_agent_module())), LazyApiCapabilities::default())
}

/// Clears the installed override so one test cannot leak into the next.
pub fn reset_devin_agent_provider_module_for_tests() {
    *MODULE_OVERRIDE.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{
        AssistantMessage, AssistantMessageEvent, Context, InputModality, Model, ModelCost, StopReason, Usage,
    };
    use crate::utils::event_stream::create_assistant_message_event_stream;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct RecordingModule {
        calls: AtomicUsize,
    }

    impl RecordingModule {
        fn new() -> Arc<Self> {
            Arc::new(Self { calls: AtomicUsize::new(0) })
        }

        fn call_log(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ProviderStreams for RecordingModule {
        fn stream(
            &self,
            model: &Model,
            _context: &Context,
            _options: Option<crate::types::StreamOptions>,
        ) -> crate::types::AssistantMessageEventStream {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let stream = create_assistant_message_event_stream();
            let message = AssistantMessage {
                content: Vec::new(),
                api: model.api.clone(),
                provider: model.provider.clone(),
                model: model.id.clone(),
                response_model: None,
                response_id: None,
                provider_thinking_level: None,
                diagnostics: None,
                usage: Usage::default(),
                stop_reason: StopReason::Stop,
                stop_details: None,
                deferred: None,
                error_message: None,
                abort_source: None,
                raw_stop_reason: None,
                end_turn: None,
                timestamp: 0,
            };
            stream.push(AssistantMessageEvent::Done {
                reason: crate::types::DoneReason::Stop,
                message: message.clone(),
            });
            stream.end(Some(message));
            stream
        }

        fn stream_simple(
            &self,
            model: &Model,
            context: &Context,
            options: Option<crate::types::SimpleStreamOptions>,
        ) -> crate::types::AssistantMessageEventStream {
            self.stream(model, context, options.map(|simple| simple.stream))
        }
    }

    /// `createFauxCore({}).getModel()`: the wrapper is handed the faux provider's own model, not a
    /// catalog entry, so the test builds the same defaults the faux core uses.
    fn model() -> Model {
        Model {
            id: "faux-1".into(),
            name: "Faux Model".into(),
            api: "faux".into(),
            provider: "faux".into(),
            base_url: "http://localhost:0".into(),
            reasoning: false,
            thinking_level_map: None,
            input: vec![InputModality::Text, InputModality::Image],
            cost: ModelCost::default(),
            context_window: 128_000,
            max_tokens: 16_384,
            sampling_params: None,
            headers: None,
            cache_retention: None,
            upstream_model_id: None,
            service_tier: None,
            recover_text_tool_calls: None,
            compat: None,
        }
    }

    #[tokio::test]
    async fn loads_the_concrete_provider_when_no_override_is_installed() {
        reset_devin_agent_provider_module_for_tests();
        let loaded = load_devin_agent_module();
        assert!(Arc::ptr_eq(&loaded, &load_devin_agent_module()), "the concrete module is loaded once");
    }

    #[tokio::test]
    async fn prefers_the_installed_module_when_the_concrete_provider_was_already_loaded() {
        reset_devin_agent_provider_module_for_tests();
        let _ = load_devin_agent_module();
        let override_module = RecordingModule::new();
        set_devin_agent_provider_module(override_module.clone());
        assert!(Arc::ptr_eq(&load_devin_agent_module(), &(override_module.clone() as Arc<dyn ProviderStreams>)));
        reset_devin_agent_provider_module_for_tests();
    }

    #[tokio::test]
    async fn uses_the_newest_override_when_a_lazy_api_was_created_before_registration() {
        reset_devin_agent_provider_module_for_tests();
        let api = devin_agent_api();
        let previous = RecordingModule::new();
        let current = RecordingModule::new();
        set_devin_agent_provider_module(previous.clone());
        set_devin_agent_provider_module(current.clone());
        let message = api
            .stream_simple(&model(), &Context::default(), None)
            .result()
            .await
            .expect("result");
        assert_eq!(message.stop_reason, StopReason::Stop);
        assert_eq!(previous.call_log(), 0);
        assert_eq!(current.call_log(), 1);
        reset_devin_agent_provider_module_for_tests();
    }
}
