//! Port of senpi `experimental/micro/models.ts` (pin 903b8336): `createPicoModels` (Pico3's
//! `Models` over `ModelRuntime`), the request-shape adapter from Pico3's durable request to the
//! pi-ai `Context`, and the models/accounts view.
//!
//! Divergences in `createPicoModels`:
//! - senpi passes `cwd` in the request options for subprocess-backed providers. maho-ai's
//!   `ProviderRequestOptions` has no `cwd` field and no Rust provider reads one, so it is held but
//!   not forwarded.
//! - senpi's stream throws the abort reason when an aborted error event arrives under an aborted
//!   context. A Pico3 `AssistantStream` yields events only, so the stream ends there instead; the
//!   generation kind treats the missing terminal event as an error and the scheduler drops it
//!   because the context is aborted.
//! - `ModelRuntime` has no deferred fetch/cancel of its own here; both go through its `Models`
//!   collection (`ModelRuntime::models`), the same path the harness lane uses.

use std::collections::BTreeMap;

use futures::StreamExt;
use maho_agent::harness::context::Context;
use maho_agent::harness::pico3::types::{
    AssistantStream, DeferredResult, Error, LocalBoxFuture, Message as PicoMessage, Model,
    ModelRef, Models, RequestOptions, Result as PicoResult, ThinkingLevel,
};
use maho_ai::types::{
    AssistantMessageEvent, ConstrainedSampling, Context as AiContext, DeferredFetchOptions,
    DeferredHandle, ErrorReason, Message, ProviderRequestOptions, SimpleStreamOptions, StopReason,
    StreamOptions, Tool,
};
use maho_core::model_runtime::ModelRuntime;

use super::api::{MicroAuthType, MicroModelSummary, MicroModelsView, MicroProviderAccount};

/// senpi `ModelToolMetadata`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelToolMetadata {
    pub constrained_sampling: Option<ConstrainedSampling>,
}

/// senpi `createPicoModels`.
pub struct PicoModels {
    runtime: ModelRuntime,
    tool_metadata: BTreeMap<String, ModelToolMetadata>,
    /// Held for parity; see the module note on `cwd`.
    pub cwd: String,
}

/// senpi `createPicoModels(runtime, toolMetadata, cwd)`.
pub fn create_pico_models(
    runtime: ModelRuntime,
    tool_metadata: BTreeMap<String, ModelToolMetadata>,
    cwd: impl Into<String>,
) -> PicoModels {
    PicoModels {
        runtime,
        tool_metadata,
        cwd: cwd.into(),
    }
}

fn reasoning(level: ThinkingLevel) -> Option<maho_ai::types::ThinkingLevel> {
    use maho_ai::types::ThinkingLevel as Reasoning;
    match level {
        ThinkingLevel::Off => None,
        ThinkingLevel::Minimal => Some(Reasoning::Minimal),
        ThinkingLevel::Low => Some(Reasoning::Low),
        ThinkingLevel::Medium => Some(Reasoning::Medium),
        ThinkingLevel::High => Some(Reasoning::High),
        ThinkingLevel::Xhigh => Some(Reasoning::Xhigh),
        ThinkingLevel::Max => Some(Reasoning::Max),
    }
}

fn request_options(ctx: &Context) -> ProviderRequestOptions {
    ProviderRequestOptions {
        signal: ctx.abort_signal(),
        ..ProviderRequestOptions::default()
    }
}

fn provider_error(message: String) -> Error {
    Error::Chord(message.into())
}

impl Models for PicoModels {
    fn resolve(&self, r#ref: &ModelRef) -> Option<Model> {
        self.runtime.get_model(&r#ref.provider, &r#ref.model_id)
    }

    fn stream(&self, model: &Model, request: RequestOptions, ctx: &Context) -> AssistantStream {
        let context = match to_ai_context(&request, &self.tool_metadata) {
            Ok(context) => context,
            Err(error) => {
                let error = maho_ai::utils::lazy::setup_error_message(model, &error);
                return futures::stream::iter([Ok(AssistantMessageEvent::Error {
                    reason: ErrorReason::Error,
                    error,
                })])
                .boxed_local();
            }
        };
        let source = self.runtime.stream_simple(
            model,
            &context,
            Some(SimpleStreamOptions {
                stream: StreamOptions {
                    request: request_options(ctx),
                    ..StreamOptions::default()
                },
                reasoning: reasoning(request.thinking_level),
                ..SimpleStreamOptions::default()
            }),
        );
        let (model, ctx) = (model.clone(), ctx.clone());
        futures::stream::unfold(Some(source), move |source| {
            let (model, ctx) = (model.clone(), ctx.clone());
            async move {
                let source = source?;
                match source.next().await {
                    Ok(Some(AssistantMessageEvent::Error {
                        reason: ErrorReason::Aborted,
                        ..
                    })) if ctx.is_aborted() => None,
                    Ok(Some(event)) => Some((Ok(event), Some(source))),
                    Ok(None) => None,
                    Err(error) => {
                        let error =
                            maho_ai::utils::lazy::setup_stream_error_message(&model, &error);
                        Some((
                            Ok(AssistantMessageEvent::Error {
                                reason: ErrorReason::Error,
                                error,
                            }),
                            None,
                        ))
                    }
                }
            }
        })
        .boxed_local()
    }

    fn fetch_deferred<'a>(
        &'a self,
        model: &'a Model,
        handle: &'a DeferredHandle,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, PicoResult<DeferredResult>> {
        Box::pin(async move {
            let options = DeferredFetchOptions {
                request: request_options(ctx),
                wait: None,
            };
            let message = self
                .runtime
                .models()
                .fetch_deferred(model, handle, Some(options), Default::default())
                .await
                .map_err(|error| provider_error(error.message))?;
            Ok(
                match message
                    .deferred
                    .clone()
                    .filter(|_| message.stop_reason == StopReason::Deferred)
                {
                    Some(deferred) => DeferredResult::Deferred(deferred),
                    None => DeferredResult::Message(Box::new(message)),
                },
            )
        })
    }

    fn cancel_deferred<'a>(
        &'a self,
        model: &'a Model,
        handle: &'a DeferredHandle,
        ctx: &'a Context,
    ) -> LocalBoxFuture<'a, PicoResult<()>> {
        Box::pin(async move {
            self.runtime
                .models()
                .cancel_deferred(model, handle, Some(request_options(ctx)))
                .await
                .map_err(provider_error)
        })
    }
}

/// senpi `toAiContext`: system messages become the joined system prompt and the running tool set
/// (removals before additions, per message); every other message passes through.
pub fn to_ai_context(
    request: &RequestOptions,
    tool_metadata: &BTreeMap<String, ModelToolMetadata>,
) -> Result<AiContext, String> {
    let mut system_prompt: Vec<String> = Vec::new();
    let mut messages: Vec<Message> = Vec::new();
    let mut tools: Vec<Tool> = Vec::new();
    for stored in &request.messages {
        let json = serde_json::to_value(stored.as_json()).map_err(|error| error.to_string())?;
        if json.get("role").and_then(serde_json::Value::as_str) != Some("system") {
            messages.push(serde_json::from_value(json).map_err(|error| error.to_string())?);
            continue;
        }
        let PicoMessage::System(system) =
            serde_json::from_value::<PicoMessage>(json).map_err(|error| error.to_string())?
        else {
            continue;
        };
        if !system.content.is_empty() {
            system_prompt.push(system.content);
        }
        for removed in system.tools_removed.unwrap_or_default() {
            tools.retain(|tool| tool.name != removed.name);
        }
        for added in system.tools_added.unwrap_or_default() {
            let parameters =
                serde_json::to_value(&added.parameters).map_err(|error| error.to_string())?;
            let tool = Tool {
                name: added.name.clone(),
                description: added.description,
                parameters,
                output_schema: None,
                freeform: None,
                constrained_sampling: tool_metadata
                    .get(&added.name)
                    .and_then(|metadata| metadata.constrained_sampling.clone()),
            };
            // `Map.set` keeps the first insertion position of a key.
            match tools
                .iter_mut()
                .find(|existing| existing.name == added.name)
            {
                Some(existing) => *existing = tool,
                None => tools.push(tool),
            }
        }
    }
    Ok(AiContext {
        system_prompt: (!system_prompt.is_empty()).then(|| system_prompt.join("\n\n")),
        messages,
        tools: (!tools.is_empty()).then_some(tools),
        active_tool_names: None,
    })
}

/// senpi `readModelsView`.
pub fn read_models_view(runtime: &ModelRuntime, refreshing: bool) -> MicroModelsView {
    let models = runtime
        .get_available_snapshot()
        .into_iter()
        .map(|model| MicroModelSummary {
            provider: model.provider,
            model_id: model.id,
            name: model.name,
            context_window: model.context_window,
            max_tokens: model.max_tokens,
        })
        .collect();
    let mut accounts = Vec::new();
    for provider in runtime.get_providers() {
        let status = runtime.get_provider_auth_status(provider.id());
        let source = status.label.or(status.source);
        let shared = |auth_type, interactive, method_name: &str| MicroProviderAccount {
            id: provider.id().to_owned(),
            name: provider.name().to_owned(),
            auth_type,
            configured: status.configured,
            source: source.clone(),
            interactive,
            method_name: Some(method_name.to_owned()),
        };
        let Some(auth) = provider.auth() else {
            continue;
        };
        if let Some(oauth) = &auth.oauth {
            accounts.push(shared(MicroAuthType::Oauth, true, oauth.name()));
        }
        if let Some(api_key) = &auth.api_key {
            accounts.push(shared(
                MicroAuthType::ApiKey,
                api_key.has_login(),
                api_key.name(),
            ));
        }
    }
    let collator = icu_collator::Collator::try_new(Default::default(), Default::default())
        .expect("compiled collation data is available");
    accounts.sort_by(|left, right| collator.compare(&left.name, &right.name));
    MicroModelsView {
        models,
        accounts,
        refreshing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_agent::harness::pico3::types::{
        JsonObject, SystemMessage, SystemTool, ThinkingLevel, ToolReference, to_stored,
    };
    use maho_ai::types::{UserContent, UserMessage};

    fn tool(name: &str) -> SystemTool {
        SystemTool {
            name: name.to_owned(),
            description: format!("{name} tool"),
            parameters: JsonObject::new(),
        }
    }

    fn system(
        content: &str,
        added: Vec<SystemTool>,
        removed: Vec<&str>,
    ) -> maho_agent::harness::pico3::types::StoredMessage {
        to_stored(&PicoMessage::System(SystemMessage {
            content: content.to_owned(),
            tools_added: Some(added),
            tools_removed: Some(
                removed
                    .into_iter()
                    .map(|name| ToolReference {
                        name: name.to_owned(),
                    })
                    .collect(),
            ),
            timestamp: 1,
        }))
        .expect("system message")
    }

    #[test]
    fn system_messages_fold_into_prompt_and_tool_set() {
        let user = to_stored(&PicoMessage::User(UserMessage {
            content: UserContent::Text("hi".to_owned()),
            timestamp: 2,
        }))
        .expect("user message");
        let request = RequestOptions {
            messages: vec![
                system("base", vec![tool("read"), tool("bash")], vec![]),
                user,
                system("", vec![tool("write")], vec!["read"]),
                system("more", vec![tool("bash")], vec![]),
            ],
            thinking_level: ThinkingLevel::Off,
        };
        let metadata = BTreeMap::from([(
            "bash".to_owned(),
            ModelToolMetadata {
                constrained_sampling: Some(ConstrainedSampling::Disabled(false)),
            },
        )]);
        let context = to_ai_context(&request, &metadata).expect("context");
        assert_eq!(context.system_prompt.as_deref(), Some("base\n\nmore"));
        assert_eq!(context.messages.len(), 1);
        assert!(matches!(context.messages[0], Message::User(_)));
        let tools = context.tools.expect("tools");
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert_eq!(names, ["bash", "write"]);
        assert_eq!(
            tools[0].constrained_sampling,
            Some(ConstrainedSampling::Disabled(false))
        );
        assert_eq!(tools[1].constrained_sampling, None);
    }

    #[test]
    fn requests_without_system_content_omit_prompt_and_tools() {
        let request = RequestOptions {
            messages: vec![system("", vec![], vec![])],
            thinking_level: ThinkingLevel::Off,
        };
        let context = to_ai_context(&request, &BTreeMap::new()).expect("context");
        assert_eq!(context.system_prompt, None);
        assert!(context.messages.is_empty());
        assert_eq!(context.tools, None);
    }

    fn faux_runtime() -> (maho_ai::providers::faux::FauxProviderHandle, ModelRuntime) {
        use maho_ai::providers::faux::{RegisterFauxProviderOptions, faux_provider};
        let provider = faux_provider(RegisterFauxProviderOptions {
            tokens_per_second: Some(0.0),
            ..Default::default()
        })
        .expect("faux provider");
        let mut runtime =
            ModelRuntime::create_sync(maho_core::model_runtime::CreateModelRuntimeOptions {
                providers: Some(Vec::new()),
                ..Default::default()
            })
            .expect("model runtime");
        runtime.register_native_provider(provider.provider.clone());
        runtime
            .credentials
            .set(
                "faux",
                Some(serde_json::json!({"type": "api_key", "key": "fixture-key"})),
            )
            .expect("faux auth");
        (provider, runtime)
    }

    #[tokio::test]
    async fn pico_models_resolve_and_stream_through_the_runtime() {
        use maho_ai::providers::faux::faux_assistant_message;
        let (provider, runtime) = faux_runtime();
        provider.set_responses(vec![
            faux_assistant_message("hello", Default::default()).into(),
        ]);
        let metadata = BTreeMap::from([(
            "read".to_owned(),
            ModelToolMetadata {
                constrained_sampling: Some(ConstrainedSampling::Disabled(false)),
            },
        )]);
        let models = create_pico_models(runtime, metadata, "/work");
        assert!(
            models
                .resolve(&ModelRef {
                    provider: "faux".into(),
                    model_id: "missing".into(),
                })
                .is_none()
        );
        let model = models
            .resolve(&ModelRef {
                provider: "faux".into(),
                model_id: "faux-1".into(),
            })
            .expect("resolved model");
        let user = to_stored(&PicoMessage::User(UserMessage {
            content: UserContent::Text("hi".to_owned()),
            timestamp: 2,
        }))
        .expect("user message");
        let request = RequestOptions {
            messages: vec![system("base", vec![tool("read")], vec![]), user],
            thinking_level: ThinkingLevel::Off,
        };
        let ctx = maho_agent::harness::context::background_context();
        let events: Vec<AssistantMessageEvent> = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            models.stream(&model, request, &ctx).collect::<Vec<_>>(),
        )
        .await
        .expect("bounded stream")
        .into_iter()
        .collect::<Result<_, _>>()
        .expect("stream events");
        let Some(AssistantMessageEvent::Done { message, .. }) = events.last() else {
            panic!("stream ends with done: {events:?}");
        };
        assert_eq!(message.stop_reason, StopReason::Stop);
        let log = provider.get_call_log();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].context.system_prompt.as_deref(), Some("base"));
        let tools = log[0].context.tools.as_ref().expect("tools");
        assert_eq!(tools[0].name, "read");
        assert_eq!(
            tools[0].constrained_sampling,
            Some(ConstrainedSampling::Disabled(false))
        );
    }

    #[tokio::test]
    async fn pico_models_forward_deferred_fetch_and_cancel() {
        let (provider, runtime) = faux_runtime();
        let models = create_pico_models(runtime, BTreeMap::new(), "/work");
        let model = models
            .resolve(&ModelRef {
                provider: "faux".into(),
                model_id: "faux-1".into(),
            })
            .expect("resolved model");
        let handle = DeferredHandle {
            provider: model.provider.clone(),
            model_id: model.id.clone(),
            api: model.api.clone(),
            id: "unknown".into(),
            expires_at: None,
            poll_after_ms: None,
            data: None,
        };
        let ctx = maho_agent::harness::context::background_context();
        let fetched = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            models.fetch_deferred(&model, &handle, &ctx),
        )
        .await
        .expect("bounded fetch")
        .expect("fetch settles with a message");
        let DeferredResult::Message(message) = fetched else {
            panic!("an unknown handle settles instead of staying deferred");
        };
        assert_eq!(message.stop_reason, StopReason::Error);
        assert_eq!(provider.state().deferred_fetch_count, 1);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            models.cancel_deferred(&model, &handle, &ctx),
        )
        .await
        .expect("bounded cancel")
        .expect("cancel forwarded");
        assert_eq!(provider.state().cancelled_deferred, vec![handle]);
    }
}
