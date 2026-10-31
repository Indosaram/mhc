//! Port of senpi `experimental/micro/models.ts` (pin 903b8336): the request-shape adapter from
//! Pico3's durable request to `ModelRuntime`'s pi-ai `Context`, and the models/accounts view.
//!
//! senpi's `createPicoModels` (the `Models` trait object over `ModelRuntime`) is not here: Pico3's
//! `Models::stream` returns `AssistantStream = Pin<Box<dyn futures::Stream>>`, and maho-cli has no
//! direct `futures` dependency to implement that trait (registry request). `to_ai_context` below is
//! the request half it would call.

use std::collections::BTreeMap;

use maho_agent::harness::pico3::types::{Message as PicoMessage, RequestOptions};
use maho_ai::types::{ConstrainedSampling, Context as AiContext, Message, Tool};
use maho_core::model_runtime::ModelRuntime;

use super::api::{MicroAuthType, MicroModelSummary, MicroModelsView, MicroProviderAccount};

/// senpi `ModelToolMetadata`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelToolMetadata {
    pub constrained_sampling: Option<ConstrainedSampling>,
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
}
