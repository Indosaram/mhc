use crate::contract::ProviderEnv;
use maho_ai::model::Model;
use maho_ai::types::{AssistantMessageEventStream, Context, SimpleStreamOptions, ThinkingLevel};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum CommandCodeTransport {
    #[default]
    Unknown,
    Provider,
    Generate,
}

pub fn is_upgrade_required(status: u16, body: &serde_json::Value) -> bool {
    if status != 403 {
        return false;
    }
    let inner = body.get("error").unwrap_or(body);
    inner.get("code").and_then(serde_json::Value::as_str) == Some("upgrade_required")
}

#[derive(Default)]
pub struct TransportRouter {
    by_key: Mutex<BTreeMap<String, CommandCodeTransport>>,
}

impl TransportRouter {
    pub fn transport_for(&self, key: &str) -> CommandCodeTransport {
        self.by_key
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(key)
            .copied()
            .unwrap_or_default()
    }

    pub fn set(&self, key: &str, transport: CommandCodeTransport) {
        self.by_key
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key.to_owned(), transport);
    }

    pub fn note_upgrade_required(&self, key: &str) {
        self.set(key, CommandCodeTransport::Generate);
    }

    pub fn reset(&self, key: &str) {
        self.by_key
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(key);
    }
}

pub fn decide(router: &TransportRouter, key: &str, upgrade_observed: bool) -> CommandCodeTransport {
    if router.transport_for(key) == CommandCodeTransport::Generate {
        return CommandCodeTransport::Generate;
    }
    if upgrade_observed {
        router.note_upgrade_required(key);
        return CommandCodeTransport::Generate;
    }
    router.set(key, CommandCodeTransport::Provider);
    CommandCodeTransport::Provider
}

fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|value| !value.trim().is_empty())
}

// Every registered model carries the synthetic `commandcode-custom` api so the registry routes it
// here; the real wire protocol is chosen per model id. Do not "fix" api to a literal protocol.
pub fn request_target(model: &Model, env: &ProviderEnv) -> Model {
    let mut target = model.clone();
    target.api = crate::catalog::api_for_model_id(&model.id).to_owned();
    target.base_url = crate::contract::base_url_for_model(&env.api_base, &target.api);
    target
}

pub fn apply_request_headers(
    options: &mut SimpleStreamOptions,
    env: &ProviderEnv,
    api_key: Option<String>,
) {
    options.stream.request.api_key = api_key;
    let headers = options
        .stream
        .request
        .headers
        .get_or_insert_with(Default::default);
    let mut insert = |name: &str, value: String| {
        headers.insert(name.to_owned(), Some(value));
    };
    insert("User-Agent", "cli".to_owned());
    insert(
        "x-command-code-version",
        crate::catalog::COMMAND_CODE_CLI_VERSION.to_owned(),
    );
    insert("x-cli-environment", "production".to_owned());
    insert("x-project-slug", crate::contract::project_slug(&env.cwd));
    if zdr_enabled(
        std::env::var("CMD_ZDR").ok().as_deref(),
        std::env::var("COMMANDCODE_ZDR").ok().as_deref(),
    ) {
        insert("x-cmd-zdr", "1".to_owned());
    }
    if let Some(session) = options
        .stream
        .request
        .affinity_session_id
        .clone()
        .filter(|value| uuid::Uuid::parse_str(value).is_ok())
    {
        insert("x-session-id", session);
    }
}

pub fn zdr_enabled(cmd_zdr: Option<&str>, commandcode_zdr: Option<&str>) -> bool {
    [cmd_zdr, commandcode_zdr]
        .into_iter()
        .flatten()
        .any(|value| value == "1")
}

fn delegate(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
    env: &ProviderEnv,
    api_key: Option<String>,
) -> AssistantMessageEventStream {
    let context = context.clone();
    let forwarding = options
        .as_ref()
        .and_then(|options| options.stream.request.signal.clone());
    let mut options = options.unwrap_or_default();
    apply_request_headers(&mut options, env, api_key.clone());
    let target = request_target(model, env);
    let api = target.api.clone();
    maho_ai::api::lazy::lazy_stream(model, async move {
        let provider = maho_ai::api_registry::get_api_provider(&api)
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("API provider {api} is not registered"))?;
        let _ = &forwarding;
        Ok(provider.stream_simple(&target, &context, Some(options)))
    })
    .into_stream()
}

pub fn stream_simple(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
) -> AssistantMessageEventStream {
    let env = ProviderEnv::from_process();
    let explicit = options
        .as_ref()
        .and_then(|options| options.stream.request.api_key.clone())
        .filter(|key| !crate::api_key::is_placeholder(key));
    let api_key = non_empty(explicit).or_else(|| crate::api_key::get_configured_api_key(&env));
    let Some(key) = api_key else {
        let missing = "No Command Code API key. Run /login and select Command Code, set COMMAND_CODE_API_KEY (or legacy COMMANDCODE_API_KEY), or configure ~/.commandcode/auth.json, ~/.pi/agent/auth.json or ~/.omp/agent/auth.json".to_owned();
        return maho_ai::api::lazy::lazy_stream(model, async move { Err(missing) }).into_stream();
    };
    delegate(model, context, options, &env, Some(key))
}

pub fn reasoning_effort(model: &Model, level: ThinkingLevel) -> Option<String> {
    let map = model.thinking_level_map.as_ref()?;
    let mapped = map.get(&maho_ai::types::ModelThinkingLevel::from(level))?;
    mapped.clone()
}

/// Resolves the key the same way both transports do, so a key switch can reset routing.
fn resolve_key(options: Option<&SimpleStreamOptions>, env: &ProviderEnv) -> Option<String> {
    options
        .and_then(|options| options.stream.request.api_key.clone())
        .filter(|key| !crate::api_key::is_placeholder(key))
        .filter(|key| !key.trim().is_empty())
        .or_else(|| crate::api_key::get_configured_api_key(env))
}

/// True when a provider error is the gateway 403 `upgrade_required` rejection.
pub fn upgrade_required_error(message: &str) -> bool {
    message.contains("upgrade_required")
}

/// The provider stream: the native leg until the gateway answers `upgrade_required`, then the
/// `/alpha/generate` fallback for every later request with that key.
pub fn stream_with_fallback(
    model: &Model,
    context: &Context,
    options: Option<SimpleStreamOptions>,
    env: &ProviderEnv,
    router: Arc<TransportRouter>,
) -> AssistantMessageEventStream {
    let key = resolve_key(options.as_ref(), env);
    let key_id = key.clone().unwrap_or_default();
    if router.transport_for(&key_id) == CommandCodeTransport::Generate {
        return crate::core::stream_generate(model, context, options_with_key(options, key), env);
    }
    let output = maho_ai::utils::event_stream::create_assistant_message_event_stream();
    let target = output.clone();
    let model = model.clone();
    let context = context.clone();
    let env = env.clone();
    tokio::spawn(async move {
        let source = delegate(&model, &context, options, &env, key.clone());
        let mut upgraded = false;
        while let Ok(Some(event)) = source.next().await {
            match event {
                maho_ai::types::AssistantMessageEvent::Error { error, .. }
                    if !upgraded
                        && error
                            .error_message
                            .as_deref()
                            .is_some_and(upgrade_required_error) =>
                {
                    upgraded = true;
                    break;
                }
                other => target.push(other),
            }
        }
        if upgraded {
            router.note_upgrade_required(&key_id);
            let fallback = crate::core::stream_generate(&model, &context, None, &env);
            while let Ok(Some(event)) = fallback.next().await {
                target.push(event);
            }
        }
        target.end(None);
    });
    output
}

fn options_with_key(
    options: Option<SimpleStreamOptions>,
    key: Option<String>,
) -> Option<SimpleStreamOptions> {
    let mut options = options?;
    if options.stream.request.api_key.is_none() {
        options.stream.request.api_key = key;
    }
    Some(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upgrade_required_needs_a_403_and_the_code() {
        assert!(is_upgrade_required(
            403,
            &json!({"error": {"code": "upgrade_required"}})
        ));
        assert!(is_upgrade_required(
            403,
            &json!({"code": "upgrade_required"})
        ));
        assert!(!is_upgrade_required(
            500,
            &json!({"error": {"code": "upgrade_required"}})
        ));
        assert!(!is_upgrade_required(
            403,
            &json!({"error": {"code": "other"}})
        ));
        assert!(!is_upgrade_required(403, &json!({})));
    }

    #[test]
    fn an_upgraded_key_stays_on_the_generate_transport() {
        let router = TransportRouter::default();
        assert_eq!(
            decide(&router, "key", false),
            CommandCodeTransport::Provider
        );
        assert_eq!(decide(&router, "key", true), CommandCodeTransport::Generate);
        assert_eq!(
            decide(&router, "key", false),
            CommandCodeTransport::Generate
        );
        assert_eq!(router.transport_for("other"), CommandCodeTransport::Unknown);
    }

    #[test]
    fn zero_data_retention_headers() {
        assert!(zdr_enabled(Some("1"), None));
        assert!(zdr_enabled(None, Some("1")));
        assert!(!zdr_enabled(Some("0"), None));
        assert!(!zdr_enabled(None, None));
    }

    #[test]
    fn a_model_targets_the_protocol_implied_by_its_id() {
        let env = ProviderEnv {
            api_base: "https://api.commandcode.ai/provider/v1".into(),
            models_url: String::new(),
            models_timeout_ms: 1,
            cache_path: "/tmp/x".into(),
            cwd: "/tmp".into(),
            home: "/tmp".into(),
        };
        let model: Model = serde_json::from_value(serde_json::json!({
            "id": "claude-opus-5",
            "name": "Claude Opus 5",
            "api": "commandcode-custom",
            "provider": "commandcode",
            "baseUrl": "",
            "reasoning": false,
            "input": ["text"],
            "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0},
            "contextWindow": 1,
            "maxTokens": 1
        }))
        .expect("model fixture");
        let target = request_target(&model, &env);
        assert_eq!(target.api, "anthropic-messages");
        assert_eq!(target.base_url, "https://api.commandcode.ai/provider");
        assert_eq!(
            crate::catalog::api_for_model_id("gpt-5.4"),
            "openai-completions"
        );
    }
}
