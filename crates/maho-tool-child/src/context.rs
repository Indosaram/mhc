//! senpi `modes/rpc/tool-child/context.ts` (pin 26acd9353).
//!
//! What a tool body running in the tool child may see of its extension context: plain, serializable data only.
//! No functions, no `ui`, no `session_manager` - those stay on the server and are reached through host calls.
use std::path::PathBuf;

use maho_ext_api::{ExtensionContext, ExtensionMode, ServiceTier};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// `BrowserEngine` of senpi `core/browser-engine.ts` (`BROWSER_ENGINES`): which browser a session's skills drive.
/// `ExtensionContext` has no browser-engine field in maho-ext-api, so a host supplies it through
/// `NarrowableContext::browser_engine`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BrowserEngine {
    Connected,
    Builtin,
    None,
}

/// `ExtensionMode` on the wire: `"tui" | "rpc" | "json" | "print"`.
fn mode_str(mode: ExtensionMode) -> &'static str {
    match mode {
        ExtensionMode::Tui => "tui",
        ExtensionMode::Rpc => "rpc",
        ExtensionMode::Json => "json",
        ExtensionMode::Print => "print",
    }
}

fn serialize_mode<S: Serializer>(mode: &ExtensionMode, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(mode_str(*mode))
}

fn deserialize_mode<'de, D: Deserializer<'de>>(deserializer: D) -> Result<ExtensionMode, D::Error> {
    let text = String::deserialize(deserializer)?;
    match text.as_str() {
        "tui" => Ok(ExtensionMode::Tui),
        "rpc" => Ok(ExtensionMode::Rpc),
        "json" => Ok(ExtensionMode::Json),
        "print" => Ok(ExtensionMode::Print),
        other => Err(serde::de::Error::custom(format!(
            "unknown extension mode {other}"
        ))),
    }
}

fn service_tier_str(tier: ServiceTier) -> &'static str {
    match tier {
        ServiceTier::Auto => "auto",
        ServiceTier::Flex => "flex",
        ServiceTier::Priority => "priority",
        ServiceTier::Ultrafast => "ultrafast",
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChildToolModel {
    pub provider: String,
    pub id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildToolContext {
    pub cwd: PathBuf,
    pub agent_dir: PathBuf,
    #[serde(
        serialize_with = "serialize_mode",
        deserialize_with = "deserialize_mode"
    )]
    pub mode: ExtensionMode,
    pub tool_call_id: String,
    pub project_trusted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<ChildToolModel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_engine: Option<BrowserEngine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_store_file: Option<PathBuf>,
}

/// The fields `narrow_context` reads; `ExtensionContext` satisfies it (below).
pub trait NarrowableContext {
    fn cwd(&self) -> PathBuf;
    fn agent_dir(&self) -> PathBuf;
    fn mode(&self) -> ExtensionMode;
    fn model(&self) -> Option<ChildToolModel>;
    fn thinking_level(&self) -> Option<String> {
        None
    }
    fn service_tier(&self) -> Option<String> {
        None
    }
    fn browser_engine(&self) -> Option<BrowserEngine> {
        None
    }
    fn goal_store_file(&self) -> Option<PathBuf> {
        None
    }
    fn is_project_trusted(&self) -> bool;
}

impl NarrowableContext for ExtensionContext {
    fn cwd(&self) -> PathBuf {
        self.cwd.clone()
    }

    fn agent_dir(&self) -> PathBuf {
        self.agent_dir.clone()
    }

    fn mode(&self) -> ExtensionMode {
        self.mode
    }

    fn model(&self) -> Option<ChildToolModel> {
        self.model.as_ref().map(|model| ChildToolModel {
            provider: model.provider.clone(),
            id: model.id.clone(),
        })
    }

    fn thinking_level(&self) -> Option<String> {
        self.thinking_level.map(|level| level.as_str().to_owned())
    }

    fn service_tier(&self) -> Option<String> {
        self.service_tier
            .map(|tier| service_tier_str(tier).to_owned())
    }

    fn goal_store_file(&self) -> Option<PathBuf> {
        self.goal_store_file.clone()
    }

    fn is_project_trusted(&self) -> bool {
        (self.is_project_trusted_fn)()
    }
}

/// `ids.session_id` is `ids: { sessionId?: string | undefined; toolCallId: string }`'s optional field.
pub fn narrow_context(
    ctx: &dyn NarrowableContext,
    session_id: Option<&str>,
    tool_call_id: &str,
) -> ChildToolContext {
    ChildToolContext {
        cwd: ctx.cwd(),
        agent_dir: ctx.agent_dir(),
        mode: ctx.mode(),
        tool_call_id: tool_call_id.to_owned(),
        project_trusted: ctx.is_project_trusted(),
        session_id: session_id.map(str::to_owned),
        model: ctx.model(),
        thinking_level: ctx.thinking_level(),
        service_tier: ctx.service_tier(),
        browser_engine: ctx.browser_engine(),
        goal_store_file: ctx.goal_store_file(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // context.ts has no unit test upstream; this pins what `narrow_context` copies and what its wire form carries.

    struct Fake {
        trusted: bool,
        full: bool,
    }

    impl NarrowableContext for Fake {
        fn cwd(&self) -> PathBuf {
            PathBuf::from("/work")
        }
        fn agent_dir(&self) -> PathBuf {
            PathBuf::from("/agent")
        }
        fn mode(&self) -> ExtensionMode {
            ExtensionMode::Rpc
        }
        fn model(&self) -> Option<ChildToolModel> {
            self.full.then(|| ChildToolModel {
                provider: "p".to_owned(),
                id: "m".to_owned(),
            })
        }
        fn thinking_level(&self) -> Option<String> {
            self.full.then(|| "high".to_owned())
        }
        fn service_tier(&self) -> Option<String> {
            self.full.then(|| "flex".to_owned())
        }
        fn browser_engine(&self) -> Option<BrowserEngine> {
            self.full.then_some(BrowserEngine::Builtin)
        }
        fn goal_store_file(&self) -> Option<PathBuf> {
            self.full.then(|| PathBuf::from("/goal.json"))
        }
        fn is_project_trusted(&self) -> bool {
            self.trusted
        }
    }

    #[test]
    fn copies_only_the_present_optional_fields() {
        let narrowed = narrow_context(
            &Fake {
                trusted: true,
                full: false,
            },
            None,
            "tc1",
        );
        let wire = serde_json::to_value(&narrowed);
        assert_eq!(
            wire.ok(),
            Some(serde_json::json!({
                "cwd": "/work", "agentDir": "/agent", "mode": "rpc", "toolCallId": "tc1", "projectTrusted": true
            }))
        );
    }

    #[test]
    fn copies_every_field_and_round_trips_on_the_wire() {
        let narrowed = narrow_context(
            &Fake {
                trusted: false,
                full: true,
            },
            Some("s1"),
            "tc2",
        );
        assert_eq!(narrowed.session_id.as_deref(), Some("s1"));
        assert_eq!(
            narrowed.model,
            Some(ChildToolModel {
                provider: "p".to_owned(),
                id: "m".to_owned()
            })
        );
        assert_eq!(narrowed.browser_engine, Some(BrowserEngine::Builtin));
        let wire = serde_json::to_value(&narrowed).unwrap_or_default();
        assert_eq!(wire["thinkingLevel"], "high");
        assert_eq!(wire["serviceTier"], "flex");
        assert_eq!(wire["browserEngine"], "builtin");
        assert_eq!(wire["goalStoreFile"], "/goal.json");
        assert_eq!(
            serde_json::from_value::<ChildToolContext>(wire).ok(),
            Some(narrowed)
        );
    }
}
