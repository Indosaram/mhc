//! Port of senpi `packages/coding-agent/src/core/model-change-origin.ts`.
//!
//! Who or what changed a session's model (senpi#2870). Recorded on every `model_change` entry, on the
//! `thinking_level_change` a switch writes, on the `model_changed` event and in the session log, so a
//! switch nobody remembers making can be attributed from the session record alone.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ModelChangeSource {
    /// A typed `/model <id>` in the terminal UI.
    Command,
    /// Enter in the model picker or the favorites picker.
    Picker,
    /// The favorites cycle key.
    Cycle,
    /// A terminal session's control endpoint `set_model`.
    Control,
    /// An RPC client's `set_model`, `cycle_model`, `set_fast_mode` or `set_thinking_level`.
    Rpc,
    /// A historical thread setting from the retired app-server compatibility mode.
    AppServer,
    /// An extension's `setModel` / `setSessionModel`; `actor` is the extension path.
    Extension,
    /// The default model chosen after a provider login.
    ProviderLogin,
    /// The retry controller moving to a fallback model, or back.
    Fallback,
    FallbackRevert,
    /// A switch held until compaction made room, applied on the next send; `actor` names its first source.
    HeldSwitch,
    /// A restored virtual-model selection re-recorded on resume.
    Restore,
    /// An SDK caller that named no source.
    Sdk,
}

impl ModelChangeSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::Picker => "picker",
            Self::Cycle => "cycle",
            Self::Control => "control",
            Self::Rpc => "rpc",
            Self::AppServer => "app-server",
            Self::Extension => "extension",
            Self::ProviderLogin => "provider-login",
            Self::Fallback => "fallback",
            Self::FallbackRevert => "fallback-revert",
            Self::HeldSwitch => "held-switch",
            Self::Restore => "restore",
            Self::Sdk => "sdk",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChangeOrigin {
    pub source: ModelChangeSource,
    /// Who issued it, where known: an extension path, an RPC client, the picker that was used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<String>,
    /// The RPC `effectId` of the model or thinking-level request; stamped on its transcript change entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_id: Option<String>,
}

impl ModelChangeOrigin {
    pub const fn new(source: ModelChangeSource) -> Self {
        Self {
            source,
            actor: None,
            effect_id: None,
        }
    }
}

pub const SDK_MODEL_CHANGE: ModelChangeOrigin = ModelChangeOrigin::new(ModelChangeSource::Sdk);

pub fn held_switch_origin(first: &ModelChangeOrigin) -> ModelChangeOrigin {
    ModelChangeOrigin {
        source: ModelChangeSource::HeldSwitch,
        actor: Some(match &first.actor {
            None => first.source.as_str().to_owned(),
            Some(actor) => format!("{}:{actor}", first.source.as_str()),
        }),
        effect_id: first.effect_id.clone(),
    }
}

/// A model's `provider/id` pair as the log names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelRef<'a> {
    pub provider: &'a str,
    pub id: &'a str,
}

pub struct ModelChangeLogInput<'a> {
    pub origin: &'a ModelChangeOrigin,
    pub from: Option<ModelRef<'a>>,
    pub to: ModelRef<'a>,
    pub during_turn: bool,
    pub persist_default: bool,
}

pub fn model_change_log_fields(input: &ModelChangeLogInput<'_>) -> Map<String, Value> {
    let mut fields = Map::new();
    fields.insert("source".into(), input.origin.source.as_str().into());
    if let Some(actor) = &input.origin.actor {
        fields.insert("actor".into(), actor.clone().into());
    }
    if let Some(from) = input.from {
        fields.insert(
            "from".into(),
            format!("{}/{}", from.provider, from.id).into(),
        );
    }
    fields.insert(
        "to".into(),
        format!("{}/{}", input.to.provider, input.to.id).into(),
    );
    fields.insert("duringTurn".into(), input.during_turn.into());
    fields.insert("persistDefault".into(), input.persist_default.into());
    fields
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn held_switch_names_its_first_source() {
        let plain = held_switch_origin(&ModelChangeOrigin::new(ModelChangeSource::Picker));
        assert_eq!(plain.source, ModelChangeSource::HeldSwitch);
        assert_eq!(plain.actor.as_deref(), Some("picker"));
        assert_eq!(plain.effect_id, None);
        let with_actor = held_switch_origin(&ModelChangeOrigin {
            source: ModelChangeSource::Extension,
            actor: Some("/ext.ts".into()),
            effect_id: Some("e1".into()),
        });
        assert_eq!(with_actor.actor.as_deref(), Some("extension:/ext.ts"));
        assert_eq!(with_actor.effect_id.as_deref(), Some("e1"));
    }

    #[test]
    fn log_fields_omit_absent_actor_and_from() {
        let fields = model_change_log_fields(&ModelChangeLogInput {
            origin: &SDK_MODEL_CHANGE,
            from: None,
            to: ModelRef {
                provider: "openai",
                id: "gpt-5",
            },
            during_turn: false,
            persist_default: true,
        });
        assert_eq!(
            Value::Object(fields),
            json!({"source":"sdk","to":"openai/gpt-5","duringTurn":false,"persistDefault":true})
        );
        let origin = ModelChangeOrigin {
            source: ModelChangeSource::Rpc,
            actor: Some("client".into()),
            effect_id: None,
        };
        let fields = model_change_log_fields(&ModelChangeLogInput {
            origin: &origin,
            from: Some(ModelRef {
                provider: "a",
                id: "b",
            }),
            to: ModelRef {
                provider: "c",
                id: "d",
            },
            during_turn: true,
            persist_default: false,
        });
        assert_eq!(fields["actor"], "client");
        assert_eq!(fields["from"], "a/b");
    }

    #[test]
    fn sources_serialize_kebab_case() {
        assert_eq!(
            serde_json::to_value(ModelChangeSource::FallbackRevert).ok(),
            Some(json!("fallback-revert"))
        );
        assert_eq!(ModelChangeSource::AppServer.as_str(), "app-server");
    }
}
