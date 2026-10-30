//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-session-commands.ts
//! (v2026.10.27-2).
//!
//! The terminal control endpoint's session controls: a model switch, a thinking level and an interrupt.
//! Each one runs the pane's own path (`/model`, the thinking-level selector, Esc) through the surface,
//! so validation, the footer and persistence are the ones the user gets typing it there.
use crate::session_control_server::{ControlCommand, failure, success};
use futures::future::BoxFuture;
use maho_ai::types::{Model, ModelThinkingLevel};
use maho_core::agent_session::AgentSession;
use serde_json::{Value, json};

pub trait TuiSessionControls: Send + Sync {
    /// The `/model` path: switches, updates the footer and records the default; errors with a
    /// refusal. Resolves to the optimized system prompt's name, if one applied.
    fn select_model(&self, model: Model) -> BoxFuture<'static, Result<Option<String>, String>>;
    /// The thinking-level selector: `remember` is its Ctrl+S (the model's remembered level).
    fn select_thinking_level(&self, level: ModelThinkingLevel, remember: bool);
    /// Esc on a running turn: queued input returns to the editor, the turn aborts and settles.
    fn interrupt_turn(&self) -> BoxFuture<'static, ()>;
}

pub const SESSION_CONTROL_COMMANDS: [&str; 5] = [
    "get_available_models",
    "get_available_thinking_levels",
    "set_model",
    "set_thinking_level",
    "interrupt",
];

pub async fn run_session_control_command(
    session: &AgentSession,
    controls: &dyn TuiSessionControls,
    command: &ControlCommand,
) -> Option<Value> {
    let id = command.get("id");
    let kind = command["type"].as_str().unwrap_or_default();
    match kind {
        "get_available_models" => Some(success(
            id,
            kind,
            Some(maho_rpc::session_control_actions_data::available_models_data(session)),
        )),
        "get_available_thinking_levels" => Some(success(
            id,
            kind,
            Some(json!({"levels": session.get_available_thinking_levels()})),
        )),
        "set_model" => Some(set_model(session, controls, command).await),
        "set_thinking_level" => {
            let level = session
                .get_available_thinking_levels()
                .into_iter()
                .find(|candidate| Some(candidate.as_str()) == command["level"].as_str());
            let Some(level) = level else {
                return Some(failure(
                    id,
                    kind,
                    &maho_rpc::session_control_actions_data::unsupported_thinking_level(
                        &command["level"],
                    ),
                ));
            };
            controls.select_thinking_level(level, command["scope"] != "turn");
            Some(success(id, kind, None))
        }
        "interrupt" => {
            let outcome = maho_rpc::session_control_actions_data::interrupt_running_turn(
                session,
                command.get("turnId"),
                || controls.interrupt_turn(),
            )
            .await;
            Some(success(
                id,
                kind,
                Some(serde_json::to_value(outcome).unwrap_or_else(|_| json!({}))),
            ))
        }
        _ => None,
    }
}

async fn set_model(
    session: &AgentSession,
    controls: &dyn TuiSessionControls,
    command: &ControlCommand,
) -> Value {
    let id = command.get("id");
    let kind = "set_model";
    let model = session
        .model_registry()
        .get_available()
        .into_iter()
        .find(|candidate| {
            Some(candidate.provider.as_str()) == command["provider"].as_str()
                && Some(candidate.id.as_str()) == command["modelId"].as_str()
        });
    let Some(model) = model else {
        return failure(
            id,
            kind,
            &format!(
                "Model not found: {}/{}",
                js_string(&command["provider"]),
                js_string(&command["modelId"])
            ),
        );
    };
    match controls.select_model(model.clone()).await {
        Ok(system_prompt_name) => {
            let mut data = serde_json::to_value(&model).unwrap_or_else(|_| json!({}));
            if let (Some(data), Some(name)) = (data.as_object_mut(), system_prompt_name) {
                data.insert("systemPromptName".into(), name.into());
            }
            success(id, kind, Some(data))
        }
        Err(error) => failure(id, kind, &error),
    }
}

/// JavaScript `String(value)` for the scalars a frame carries.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_not_found_names_the_requested_pair_like_javascript_string() {
        assert_eq!(js_string(&json!("faux")), "faux");
        assert_eq!(js_string(&Value::Null), "null");
        assert_eq!(js_string(&json!(3)), "3");
    }
}
