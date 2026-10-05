//! The host agent runtime seam (`@code-yeongyu/senpi` in TypeScript), modelled as traits.
//!
//! senpi-task never links the host runtime. Everything it needs from the host crosses these
//! traits, and every value the host hands back is untrusted JSON parsed at this boundary.

use serde_json::Value;

/// A host call failed (the TypeScript port observed this as a thrown exception).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("host call failed: {message}")]
pub struct HostError {
    pub message: String,
}

/// The host's model registry (`SenpiModelRegistryPort` in TypeScript).
///
/// `get_available` returns the auth-filtered model list the machine can actually call, while
/// `find` answers from the whole catalog. Implementations return raw JSON because the host
/// shape is untrusted: callers parse it with [`parse_registry_model`], which rejects malformed
/// entries and any entry carrying secret-like fields.
pub trait SenpiModelRegistry: Send + Sync {
    /// The available model list; any non-array value is treated as a malformed container.
    fn get_available(&self) -> Result<Value, HostError>;
    /// The catalog entry for `provider`/`model_id`, when one exists.
    fn find(&self, provider: &str, model_id: &str) -> Option<Value>;
}

/// A registry model that passed the boundary checks.
#[derive(Debug, Clone, PartialEq)]
pub struct ParsedRegistryModel {
    /// The untouched host value, handed back to the host when spawning.
    pub model: Value,
    pub provider: String,
    pub model_id: String,
    pub display_name: Option<String>,
}

/// A `provider`/`model_id` pair split from a `provider/model-id` string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedModel {
    pub provider: String,
    pub model_id: String,
}

const SECRET_LIKE_MODEL_FIELD_NAMES: [&str; 12] = [
    "accesstoken",
    "apikey",
    "auth",
    "authorization",
    "bearertoken",
    "clientsecret",
    "password",
    "privatekey",
    "privatetoken",
    "secret",
    "secretkey",
    "token",
];

fn has_secret_like_model_field(model: &serde_json::Map<String, Value>) -> bool {
    model.keys().any(|key| {
        let normalized: String = key
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .to_lowercase();
        SECRET_LIKE_MODEL_FIELD_NAMES.contains(&normalized.as_str())
    })
}

fn is_safe_display_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.encode_utf16().count() <= 120
        && !name
            .chars()
            .any(|ch| matches!(u32::from(ch), 0x00..=0x1f | 0x7f..=0x9f))
}

/// Parses one untrusted registry value; `expected` pins the identity a `find` result must carry.
pub fn parse_registry_model(
    model: &Value,
    expected: Option<&ParsedModel>,
) -> Option<ParsedRegistryModel> {
    let object = model.as_object()?;
    if has_secret_like_model_field(object) {
        return None;
    }
    let provider = object.get("provider")?.as_str()?;
    let model_id = object.get("id")?.as_str()?;
    if provider.is_empty() || model_id.is_empty() {
        return None;
    }
    if let Some(expected) = expected
        && (provider != expected.provider || model_id != expected.model_id)
    {
        return None;
    }
    let display_name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| is_safe_display_name(name))
        .map(str::to_string);
    Some(ParsedRegistryModel {
        model: model.clone(),
        provider: provider.to_string(),
        model_id: model_id.to_string(),
        display_name,
    })
}

/// Splits `provider/model-id`; both halves must be non-empty.
pub fn parse_model(model: &str) -> Option<ParsedModel> {
    let separator = model.find('/')?;
    if separator == 0 || separator == model.len() - 1 {
        return None;
    }
    Some(ParsedModel {
        provider: model[..separator].to_string(),
        model_id: model[separator + 1..].to_string(),
    })
}

/// The available registry list as sorted `provider/model-id` strings; `None` for a non-array.
pub fn parse_available_models(models: &Value) -> Option<Vec<String>> {
    let entries = models.as_array()?;
    let mut parsed: Vec<String> = entries
        .iter()
        .filter_map(|entry| parse_registry_model(entry, None))
        .map(|model| format!("{}/{}", model.provider, model.model_id))
        .collect();
    parsed.sort();
    Some(parsed)
}

#[cfg(test)]
pub(crate) mod fake {
    use serde_json::{Value, json};

    use super::{HostError, SenpiModelRegistry};

    /// In-crate fake of the host registry used by tests.
    pub(crate) struct FakeRegistry {
        pub(crate) available: Result<Value, HostError>,
        pub(crate) catalog: Vec<Value>,
        pub(crate) find_override: Option<Value>,
    }

    pub(crate) fn model(provider: &str, id: &str) -> Value {
        json!({ "provider": provider, "id": id })
    }

    /// `getAvailable` and `find` both answer from `models`.
    pub(crate) fn registry(models: Vec<Value>) -> FakeRegistry {
        FakeRegistry {
            available: Ok(Value::Array(models.clone())),
            catalog: models,
            find_override: None,
        }
    }

    /// `find` answers from the whole catalog while `getAvailable` is auth-filtered.
    pub(crate) fn catalog_registry(available: Vec<Value>, catalog: Vec<Value>) -> FakeRegistry {
        FakeRegistry {
            available: Ok(Value::Array(available)),
            catalog,
            find_override: None,
        }
    }

    /// `getAvailable` returns `available` verbatim and `find` always returns `find_result`.
    pub(crate) fn fixed_registry(available: Value, find_result: Option<Value>) -> FakeRegistry {
        FakeRegistry {
            available: Ok(available),
            catalog: Vec::new(),
            find_override: find_result.or(Some(Value::Null)),
        }
    }

    pub(crate) fn failing_registry(message: &str) -> FakeRegistry {
        FakeRegistry {
            available: Err(HostError {
                message: message.to_string(),
            }),
            catalog: Vec::new(),
            find_override: Some(Value::Null),
        }
    }

    impl SenpiModelRegistry for FakeRegistry {
        fn get_available(&self) -> Result<Value, HostError> {
            self.available.clone()
        }

        fn find(&self, provider: &str, model_id: &str) -> Option<Value> {
            if let Some(result) = &self.find_override {
                return (!result.is_null()).then(|| result.clone());
            }
            self.catalog
                .iter()
                .find(|candidate| {
                    candidate.get("provider").and_then(Value::as_str) == Some(provider)
                        && candidate.get("id").and_then(Value::as_str) == Some(model_id)
                })
                .cloned()
        }
    }
}
