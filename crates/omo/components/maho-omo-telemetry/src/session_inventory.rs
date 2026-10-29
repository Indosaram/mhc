//! Port of `packages/omo-senpi/src/components/telemetry/session-inventory.ts`.
//!
//! The engine's connected-model inventory, reduced to counts plus the masked default pair. A provider counts
//! when the user signed into it (`auth.json` keys) or configured it (`models.json`). Provider ids ship only
//! when they are in the known vocabulary, the default pair is masked, and no model name a user configured
//! privately leaves the machine. A missing or malformed models/settings inventory reports one diagnostic and
//! yields zero configured providers (the signed-in ones still count).

use std::path::Path;

use serde_json::Value;

use crate::model_vocabulary::KNOWN_PROVIDERS;
use crate::product_identity::mask_provider_and_model;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryDiagnosticEvent {
    ReadFailed,
}

impl InventoryDiagnosticEvent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadFailed => "omo_native_inventory_read_failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmoNativeInventoryDiagnostic {
    pub event: &'static str,
    pub error: String,
    pub source: &'static str,
}

pub type InventoryDiagnostics =
    std::sync::Arc<dyn Fn(&OmoNativeInventoryDiagnostic) + Send + Sync>;

/// Subscription ids the engine renamed; installs that predate the rename still carry the old key.
pub const RETIRED_PROVIDER_IDS: &[(&str, &str)] = &[
    ("claude-sdk-oauth", "anthropic-subscription"),
    ("openai-codex", "chatgpt-subscription"),
];

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OmoNativeInventory {
    pub default_model: Option<String>,
    pub default_provider: Option<String>,
    pub model_count: u64,
    /// Distinct providers usable this session: signed in or configured.
    pub provider_count: u64,
    pub signed_in_provider_count: u64,
    pub configured_provider_count: u64,
    /// Sorted provider kinds; any id outside the known vocabulary is folded into one `custom`.
    pub providers: String,
}

pub fn read_omo_native_inventory(
    agent_dir: &Path,
    diagnostics: Option<&InventoryDiagnostics>,
) -> OmoNativeInventory {
    let signed_in = read_signed_in_providers(agent_dir, diagnostics);
    let failed = |error: &str| {
        if let Some(diagnostics) = diagnostics {
            diagnostics(&OmoNativeInventoryDiagnostic {
                event: InventoryDiagnosticEvent::ReadFailed.as_str(),
                error: error.to_string(),
                source: "omo-native-session",
            });
        }
    };
    let models = match read_object(&agent_dir.join("models.json")) {
        Ok(models) => models,
        Err(error) => {
            failed(&error);
            return signed_in_only(&signed_in);
        }
    };
    let settings = match read_object(&agent_dir.join("settings.json")) {
        Ok(settings) => settings,
        Err(error) => {
            failed(&error);
            return signed_in_only(&signed_in);
        }
    };
    let Some(providers) = models.get("providers").and_then(Value::as_object) else {
        failed("models.json providers must be an object");
        return signed_in_only(&signed_in);
    };
    let mut model_count = 0_u64;
    for provider in providers.values() {
        if !provider.is_object() {
            failed("models.json provider must be an object");
            return signed_in_only(&signed_in);
        }
        match provider.get("models") {
            None => {}
            Some(Value::Array(models)) => model_count += models.len() as u64,
            Some(_) => {
                failed("models.json provider models must be an array");
                return signed_in_only(&signed_in);
            }
        }
    }
    let configured: Vec<String> = providers.keys().map(|id| canonical_provider(id)).collect();
    let mut usable: Vec<String> = configured.clone();
    for provider in &signed_in {
        if !usable.contains(provider) {
            usable.push(provider.clone());
        }
    }
    let default_provider = string_property(&settings, "defaultProvider");
    let default_model = string_property(&settings, "defaultModel");
    let masked = default_provider
        .as_deref()
        .zip(default_model.as_deref())
        .map(|(provider, model)| mask_provider_and_model(provider, model));
    let mut unique_configured = configured.clone();
    unique_configured.sort();
    unique_configured.dedup();
    let mut unique_signed_in = signed_in.clone();
    unique_signed_in.sort();
    unique_signed_in.dedup();
    OmoNativeInventory {
        default_provider: masked.as_ref().map(|(provider, _)| provider.clone()),
        default_model: masked.map(|(_, model)| model),
        model_count,
        provider_count: usable.len() as u64,
        signed_in_provider_count: unique_signed_in.len() as u64,
        configured_provider_count: unique_configured.len() as u64,
        providers: provider_kinds(&usable),
    }
}

fn signed_in_only(signed_in: &[String]) -> OmoNativeInventory {
    if signed_in.is_empty() {
        return OmoNativeInventory::default();
    }
    let mut unique = signed_in.to_vec();
    unique.sort();
    unique.dedup();
    OmoNativeInventory {
        provider_count: unique.len() as u64,
        signed_in_provider_count: unique.len() as u64,
        providers: provider_kinds(&unique),
        ..Default::default()
    }
}

/// `auth.json` provider keys; a missing file is no sign-in, a malformed one reports a diagnostic and counts
/// none.
fn read_signed_in_providers(
    agent_dir: &Path,
    diagnostics: Option<&InventoryDiagnostics>,
) -> Vec<String> {
    let path = agent_dir.join("auth.json");
    if !path.exists() {
        return Vec::new();
    }
    match read_object(&path) {
        Ok(auth) => auth.keys().map(|id| canonical_provider(id)).collect(),
        Err(_) => {
            if let Some(diagnostics) = diagnostics {
                // A fixed error: a JSON parse message can quote the start of a credential.
                diagnostics(&OmoNativeInventoryDiagnostic {
                    event: InventoryDiagnosticEvent::ReadFailed.as_str(),
                    error: "auth.json is unreadable or not a JSON object".into(),
                    source: "omo-native-session",
                });
            }
            Vec::new()
        }
    }
}

pub fn canonical_provider(id: &str) -> String {
    RETIRED_PROVIDER_IDS
        .iter()
        .find(|(retired, _)| *retired == id)
        .map_or_else(|| id.to_string(), |(_, canonical)| (*canonical).to_string())
}

fn provider_kinds(ids: &[String]) -> String {
    let mut known: Vec<&str> = ids
        .iter()
        .map(String::as_str)
        .filter(|id| is_known_provider(id))
        .collect();
    known.sort_unstable();
    if known.len() == ids.len() {
        return known.join(",");
    }
    known.push("custom");
    known.join(",")
}

fn is_known_provider(value: &str) -> bool {
    KNOWN_PROVIDERS.contains(&value)
}

fn read_object(path: &Path) -> Result<serde_json::Map<String, Value>, String> {
    let text = std::fs::read_to_string(path).map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_str(&text)
        .map_err(|_| "models.json or settings.json is not valid JSON".to_string())?;
    value
        .as_object()
        .cloned()
        .ok_or_else(|| format!("{} must contain an object", path.display()))
}

fn string_property(value: &serde_json::Map<String, Value>, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn write(dir: &Path, name: &str, value: serde_json::Value) {
        std::fs::write(dir.join(name), value.to_string()).unwrap();
    }

    type RecordedDiagnostics = Arc<Mutex<Vec<(String, String)>>>;
    fn diagnostics() -> (InventoryDiagnostics, RecordedDiagnostics) {
        let recorded: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let observed = Arc::clone(&recorded);
        let sink: InventoryDiagnostics = Arc::new(move |input| {
            observed
                .lock()
                .unwrap()
                .push((input.event.to_string(), input.error.clone()));
        });
        (sink, recorded)
    }

    #[test]
    fn signed_in_only_counts_without_a_configured_inventory() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "auth.json", json!({"anthropic": {"key": "secret"}}));
        let (sink, recorded) = diagnostics();
        let inventory = read_omo_native_inventory(dir.path(), Some(&sink));
        assert_eq!(inventory.provider_count, 1);
        assert_eq!(inventory.signed_in_provider_count, 1);
        assert_eq!(inventory.configured_provider_count, 0);
        assert_eq!(inventory.providers, "anthropic");
        assert_eq!(inventory.model_count, 0);
        assert_eq!(recorded.lock().unwrap().len(), 1);
        assert!(!format!("{inventory:?}").contains("secret"));
    }

    #[test]
    fn overlap_counts_each_provider_once() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "auth.json", json!({"anthropic": {}, "openai": {}}));
        write(
            dir.path(),
            "models.json",
            json!({"providers":{"anthropic":{"models":[{},{}]},"openai":{"models":[{}]}}}),
        );
        write(dir.path(), "settings.json", json!({}));
        let inventory = read_omo_native_inventory(dir.path(), None);
        assert_eq!(inventory.provider_count, 2);
        assert_eq!(inventory.signed_in_provider_count, 2);
        assert_eq!(inventory.configured_provider_count, 2);
        assert_eq!(inventory.model_count, 3);
        assert_eq!(inventory.providers, "anthropic,openai");
    }

    #[test]
    fn retired_ids_are_canonicalized() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "auth.json",
            json!({"claude-sdk-oauth": {}, "openai-codex": {}}),
        );
        write(
            dir.path(),
            "models.json",
            json!({"providers":{"claude-sdk-oauth":{"models":[{}]}}}),
        );
        write(dir.path(), "settings.json", json!({}));
        let inventory = read_omo_native_inventory(dir.path(), None);
        assert_eq!(inventory.provider_count, 2);
        assert_eq!(inventory.signed_in_provider_count, 2);
        assert_eq!(inventory.configured_provider_count, 1);
        assert_eq!(
            inventory.providers,
            "anthropic-subscription,chatgpt-subscription"
        );
    }

    #[test]
    fn malformed_models_keep_the_signed_in_counts() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "auth.json", json!({"anthropic": {}, "openai": {}}));
        write(dir.path(), "models.json", json!("[]"));
        write(dir.path(), "settings.json", json!({}));
        let (sink, recorded) = diagnostics();
        let inventory = read_omo_native_inventory(dir.path(), Some(&sink));
        assert_eq!(inventory.provider_count, 2);
        assert_eq!(inventory.signed_in_provider_count, 2);
        assert_eq!(inventory.configured_provider_count, 0);
        assert_eq!(inventory.providers, "anthropic,openai");
        assert_eq!(recorded.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_parse_error_never_quotes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "auth.json", json!({"anthropic": {}}));
        std::fs::write(dir.path().join("models.json"), "{\"apiKey\":\"sk-secret").unwrap();
        write(dir.path(), "settings.json", json!({}));
        let (sink, recorded) = diagnostics();
        let inventory = read_omo_native_inventory(dir.path(), Some(&sink));
        assert_eq!(inventory.providers, "anthropic");
        let recorded = recorded.lock().unwrap();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].1, "models.json or settings.json is not valid JSON");
        assert!(!recorded[0].1.contains("sk-secret"));
    }

    #[test]
    fn malformed_auth_reports_a_fixed_message() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("auth.json"), "{\"token\":\"sk-secret").unwrap();
        write(dir.path(), "models.json", json!({"providers":{}}));
        write(dir.path(), "settings.json", json!({}));
        let (sink, recorded) = diagnostics();
        let inventory = read_omo_native_inventory(dir.path(), Some(&sink));
        assert_eq!(inventory.provider_count, 0);
        let recorded = recorded.lock().unwrap();
        assert_eq!(
            recorded[0].1,
            "auth.json is unreadable or not a JSON object"
        );
        assert!(!recorded[0].1.contains("sk-secret"));
    }

    #[test]
    fn default_pair_is_masked_and_private_model_names_never_ship() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "models.json",
            json!({"providers":{"anthropic":{"models":[{}]},"private-cloud":{"models":[{}]}}}),
        );
        write(
            dir.path(),
            "settings.json",
            json!({"defaultProvider":"private-cloud","defaultModel":"acme-internal-ft"}),
        );
        let inventory = read_omo_native_inventory(dir.path(), None);
        assert_eq!(inventory.default_provider.as_deref(), Some("custom"));
        assert_eq!(inventory.default_model.as_deref(), Some("custom"));
        assert_eq!(inventory.providers, "anthropic,custom");
        assert_eq!(inventory.provider_count, 2);
    }

    #[test]
    fn empty_or_partial_defaults_are_omitted() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "models.json", json!({"providers":{}}));
        write(
            dir.path(),
            "settings.json",
            json!({"defaultProvider":"anthropic","defaultModel":""}),
        );
        let inventory = read_omo_native_inventory(dir.path(), None);
        assert_eq!(inventory.default_provider, None);
        assert_eq!(inventory.default_model, None);
        assert_eq!(inventory.providers, "");
        assert_eq!(inventory.provider_count, 0);
    }

    #[test]
    fn provider_models_must_be_an_array() {
        let dir = tempfile::tempdir().unwrap();
        write(
            dir.path(),
            "models.json",
            json!({"providers":{"anthropic":{"models":{"id":"x"}}}}),
        );
        write(dir.path(), "settings.json", json!({}));
        let (sink, recorded) = diagnostics();
        let inventory = read_omo_native_inventory(dir.path(), Some(&sink));
        assert_eq!(inventory.configured_provider_count, 0);
        assert_eq!(recorded.lock().unwrap().len(), 1);
    }
}
