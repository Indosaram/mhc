use std::sync::Arc;

use maho_ai::types::{ModelThinkingLevel, ThinkingLevel};
use maho_core::agent_session::AgentSession;
use maho_ext_api::{ExtensionContext, ExtensionFailure, Model};
use maho_ext_reasoning::{ReasoningHost, ThinkingPreferences};
use serde_json::{Map, Value};

/// Native backing for the builtin reasoning extension.
///
/// The weak accessor resolves the session at each operation, so commands follow live session
/// replacement and never keep an ended parent session alive.
pub struct NativeReasoningHost {
    parent: Arc<dyn Fn() -> Option<AgentSession> + Send + Sync>,
}

impl NativeReasoningHost {
    pub fn new(parent: Arc<dyn Fn() -> Option<AgentSession> + Send + Sync>) -> Self {
        Self { parent }
    }

    fn session(&self) -> Result<AgentSession, ExtensionFailure> {
        (self.parent)().ok_or_else(|| ExtensionFailure::new("Reasoning requires a live parent session"))
    }

    fn model_level(value: Option<&Value>) -> Option<ModelThinkingLevel> {
        let value = value?.as_str()?;
        ModelThinkingLevel::ALL.into_iter().find(|level| level.as_str() == value)
    }

    pub fn remembered_level(settings: &maho_core::settings_manager::SettingsManager, model: &Model) -> Option<ModelThinkingLevel> {
        let key = Self::model_key(model);
        Self::model_level(settings.get_value("modelThinkingLevels").and_then(|levels| levels.get(&key)))
    }

    pub fn last_on_level(settings: &maho_core::settings_manager::SettingsManager, model: &Model) -> Option<ModelThinkingLevel> {
        let key = Self::model_key(model);
        Self::model_level(settings.get_value("modelLastOnThinkingLevels").and_then(|levels| levels.get(&key)))
            .filter(|level| *level != ModelThinkingLevel::Off)
    }

    pub fn default_level(settings: &maho_core::settings_manager::SettingsManager) -> Option<ModelThinkingLevel> {
        Self::model_level(settings.get_value("defaultThinkingLevel"))
    }

    fn model_key(model: &Model) -> String {
        format!("{}/{}", model.provider, model.id)
    }

    fn set_global_values(session: &AgentSession, values: Map<String, Value>) -> Result<(), ExtensionFailure> {
        session.with_settings_manager_mut(|manager| manager.set(
            maho_core::settings_manager::SettingsScope::Global,
            &values,
        )).map_err(ExtensionFailure::new)
    }

    fn remember_last_on_value(session: &AgentSession, model: &Model, level: ModelThinkingLevel) -> Result<(), ExtensionFailure> {
        let key = Self::model_key(model);
        let mut global = session.with_settings_manager(|manager| manager.get_global().clone());
        let mut last_on = global.get("modelLastOnThinkingLevels").and_then(Value::as_object).cloned().unwrap_or_default();
        last_on.insert(key, Value::String(level.as_str().to_owned()));
        global.insert("modelLastOnThinkingLevels".to_owned(), Value::Object(last_on));
        Self::set_global_values(session, Map::from_iter([(
            "modelLastOnThinkingLevels".to_owned(), global["modelLastOnThinkingLevels"].clone(),
        )]))
    }

    fn restore_preferences(session: &AgentSession, model: &Model, level: ModelThinkingLevel) -> Result<(), ExtensionFailure> {
        let key = Self::model_key(model);
        let global = session.with_settings_manager(|manager| manager.get_global().clone());
        let mut levels = global.get("modelThinkingLevels").and_then(Value::as_object).cloned().unwrap_or_default();
        levels.insert(key, Value::String(level.as_str().to_owned()));
        Self::set_global_values(session, Map::from_iter([
            ("modelThinkingLevels".to_owned(), Value::Object(levels)),
            ("defaultThinkingLevel".to_owned(), Value::String(level.as_str().to_owned())),
        ]))
    }
}

impl ReasoningHost for NativeReasoningHost {
    fn level(&self, _: &ExtensionContext) -> Result<ModelThinkingLevel, ExtensionFailure> {
        Ok(self.session()?.thinking_level())
    }

    fn set_level(&self, _: &ExtensionContext, level: ModelThinkingLevel) -> Result<(), ExtensionFailure> {
        self.session()?.set_thinking_level(level);
        Ok(())
    }

    fn preferences(&self, _: &ExtensionContext, model: &Model) -> Result<ThinkingPreferences, ExtensionFailure> {
        let session = self.session()?;
        let manager = session.with_settings_manager(|manager| {
            (Self::last_on_level(manager, model), Self::remembered_level(manager, model), Self::default_level(manager))
        });
        Ok(ThinkingPreferences {
            last_on: manager.0,
            remembered: manager.1,
            global: manager.2,
        })
    }

    fn remember_last_on<'a>(&'a self, _context: &'a ExtensionContext, model: &'a Model, level: ModelThinkingLevel) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let session = self.session()?;
            Self::remember_last_on_value(&session, model, level)?;
            Ok(())
        })
    }

    fn restore_on<'a>(&'a self, _context: &'a ExtensionContext, model: &'a Model, level: ThinkingLevel) -> maho_ext_api::ExtensionFuture<'a, ()> {
        Box::pin(async move {
            let session = self.session()?;
            let level: ModelThinkingLevel = level.into();
            Self::restore_preferences(&session, model, level)?;
            session.set_session_thinking_level(level);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use maho_core::settings_manager::{SettingsManager, SettingsScope, Settings};
    use serde_json::json;

    fn model() -> Model {
        serde_json::from_value(json!({
            "id": "model-a", "name": "Model A", "api": "openai-completions", "provider": "provider-a",
            "baseUrl": "", "reasoning": true, "input": ["text"],
            "cost": {"input": 0.0, "output": 0.0, "cacheRead": 0.0, "cacheWrite": 0.0},
            "contextWindow": 1000, "maxTokens": 100
        })).expect("model fixture")
    }

    #[test]
    fn remembering_model_level_preserves_other_models_and_default() {
        let root = tempfile::tempdir().expect("temporary settings root");
        let cwd = root.path().to_string_lossy();
        let agent_dir = root.path().join("agent").to_string_lossy().into_owned();
        let mut manager = SettingsManager::create(&cwd, &agent_dir, &cwd, false);
        manager.set(SettingsScope::Global, &Settings::from_iter([(
            "defaultThinkingLevel".to_owned(), json!("low"),
        )])).expect("write default level");
        let model = model();
        let mut model_levels = Map::from_iter([("provider-b/model-b".to_owned(), json!("xhigh"))]);
        model_levels.insert(NativeReasoningHost::model_key(&model), json!("high"));
        manager.set(SettingsScope::Global, &Settings::from_iter([
            ("modelThinkingLevels".to_owned(), Value::Object(model_levels)),
            ("modelLastOnThinkingLevels".to_owned(), json!({"provider-a/model-a":"xhigh"})),
        ])).expect("write model levels");
        let reloaded = SettingsManager::create(&cwd, &agent_dir, &cwd, false);
        assert_eq!(NativeReasoningHost::remembered_level(&reloaded, &model), Some(ModelThinkingLevel::High));
        assert_eq!(NativeReasoningHost::last_on_level(&reloaded, &model), Some(ModelThinkingLevel::Xhigh));
        assert_eq!(NativeReasoningHost::default_level(&reloaded), Some(ModelThinkingLevel::Low));
        assert_eq!(reloaded.get_value("modelThinkingLevels").and_then(|levels| levels.get("provider-b/model-b"))
            .and_then(Value::as_str), Some("xhigh"));
    }
}
