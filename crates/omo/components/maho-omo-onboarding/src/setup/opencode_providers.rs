//! Reads the custom providers (`provider.<id>` blocks) of the OpenCode user-scope config and its
//! models.dev catalog cache, and converts each into the provider entry the engine's `models.json`
//! accepts, plus the API key to store for it (upstream
//! `omo-native/bin/lib/setup-opencode-providers.js`). Read-only: nothing here writes.

use super::SetupRuntime;
use super::auth_store::literal_config_value;
use super::opencode_assets::{
    convert_placeholders, opencode_config_sources, read_opencode_section, unconverted_placeholder,
};
use super::provider_map::provider_map;
use serde_json::{Map, Value, json};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

/// The AI SDK package an OpenCode provider names decides its wire protocol.
pub fn api_by_npm(npm: &str) -> Option<&'static str> {
    match npm {
        "@ai-sdk/openai-compatible" => Some("openai-completions"),
        "@ai-sdk/anthropic" => Some("anthropic-messages"),
        "@ai-sdk/openai" => Some("openai-responses"),
        _ => None,
    }
}

// OpenCode's own fallback for a provider that names no package.
const DEFAULT_NPM: &str = "@ai-sdk/openai-compatible";
const MODEL_INPUTS: [&str; 3] = ["text", "image", "video"];

fn text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|v| !v.trim().is_empty())
}

fn positive(value: Option<&Value>) -> Option<&Value> {
    value.filter(|v| v.as_f64().is_some_and(|n| n.is_finite() && n > 0.0))
}

fn builtin_provider_ids() -> BTreeSet<String> {
    let map = provider_map();
    map.builtin_provider_ids
        .iter()
        .cloned()
        .chain(map.providers.keys().cloned())
        .collect()
}

// Literal bytes are escaped, and OpenCode's `{env:NAME}` becomes the engine's `${NAME}`. `None` when
// a placeholder is left that the engine has no spelling for.
fn config_value(value: &str) -> Option<String> {
    let converted = convert_placeholders(&literal_config_value(value));
    (!unconverted_placeholder(&Value::String(converted.clone()))).then_some(converted)
}

// @ai-sdk/anthropic posts to `<baseURL>/messages`; the engine's Anthropic client appends
// `/v1/messages` itself, so the `/v1` has to leave the base.
fn engine_base_url(api: &str, base_url: &str) -> Option<String> {
    if api != "anthropic-messages" {
        return Some(base_url.to_owned());
    }
    let trimmed = base_url.strip_suffix('/').unwrap_or(base_url);
    trimmed
        .strip_suffix("/v1")
        .filter(|head| !head.is_empty())
        .map(str::to_owned)
}

fn read_opencode_auth(runtime: &SetupRuntime, notices: &mut Vec<String>) -> Map<String, Value> {
    let path = super::credentials::opencode_auth_path(runtime);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Map::new();
    };
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(map)) => map,
        Ok(_) => Map::new(),
        Err(_) => {
            notices.push("WARN opencode: could not parse auth.json; custom provider keys stored there were not imported".into());
            Map::new()
        }
    }
}

/// A provider key and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderKey {
    pub key: String,
    pub source: String,
}

// OpenCode's order: `options.apiKey` wins, else the auth.json `api` entry, else the one env var
// `env` names - a list of several names never yields a key.
fn convert_key(
    id: &str,
    entry: &Map<String, Value>,
    auth: &Map<String, Value>,
    notices: &mut Vec<String>,
) -> Option<ProviderKey> {
    let kind = if entry.get("catalog") == Some(&Value::Bool(true)) {
        "catalog provider"
    } else {
        "custom provider"
    };
    if let Some(inline) = text(entry.get("options").and_then(|o| o.get("apiKey"))) {
        if let Some(key) = config_value(inline) {
            return Some(ProviderKey {
                key,
                source: "opencode config apiKey".into(),
            });
        }
        notices.push(format!("NOTICE opencode: {kind} {id} apiKey uses a {{file:...}} or {{env:...}} placeholder omo cannot express; its key was not imported - start omo and run /login {id}"));
        return None;
    }
    let stored = auth.get(id);
    if stored.and_then(|s| s.get("type")).and_then(Value::as_str) == Some("api")
        && let Some(key) = text(stored.and_then(|s| s.get("key")))
    {
        return Some(ProviderKey {
            key: literal_config_value(key),
            source: "opencode auth.json".into(),
        });
    }
    let names: Vec<&Value> = entry
        .get("env")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    if let [Value::String(name)] = names.as_slice() {
        let valid = {
            let mut chars = name.chars();
            matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        if valid {
            return Some(ProviderKey {
                key: format!("${{{name}}}"),
                source: format!("environment variable {name}"),
            });
        }
    }
    None
}

// A URL still holding a placeholder has no engine spelling.
fn fixed_url(value: Option<&str>) -> Option<&str> {
    value.filter(|v| !(v.contains("{env:") || v.contains("{file:") || v.contains("${")))
}

struct ProviderContext<'a> {
    id: &'a str,
    api: &'a str,
    base_url_raw: Option<&'a str>,
    base_url: &'a str,
    options_base_url: Option<&'a str>,
}

fn convert_model(
    provider: &ProviderContext<'_>,
    id: &str,
    entry: &Value,
    notices: &mut Vec<String>,
) -> Option<Value> {
    let empty = Map::new();
    let source = entry.as_object().unwrap_or(&empty);
    let npm = text(source.get("provider").and_then(|p| p.get("npm")));
    let api = match npm {
        None => provider.api,
        Some(npm) => match api_by_npm(npm) {
            Some(api) => api,
            None => {
                notices.push(format!("NOTICE opencode: model {}/{id} uses npm package {npm}, which omo has no API adapter for; not imported", provider.id));
                return None;
            }
        },
    };
    let base_url_raw = provider
        .options_base_url
        .or_else(|| text(source.get("provider").and_then(|p| p.get("api"))))
        .or(provider.base_url_raw);
    let base_url = fixed_url(base_url_raw).and_then(|url| engine_base_url(api, url));
    let Some(base_url) = base_url else {
        notices.push(format!(
            "NOTICE opencode: model {}/{id} baseURL {} is not a fixed URL omo's {api} client can use; not imported",
            provider.id,
            base_url_raw.unwrap_or("undefined")
        ));
        return None;
    };
    let input: Vec<Value> = source
        .get("modalities")
        .and_then(|m| m.get("input"))
        .and_then(Value::as_array)
        .map(|kinds| {
            kinds
                .iter()
                .filter(|k| k.as_str().is_some_and(|k| MODEL_INPUTS.contains(&k)))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    let mut model = Map::new();
    model.insert("id".into(), json!(id));
    if let Some(upstream) = text(source.get("id")).filter(|u| *u != id) {
        model.insert("upstreamModelId".into(), json!(upstream));
    }
    if let Some(name) = text(source.get("name")) {
        model.insert("name".into(), json!(name));
    }
    if api != provider.api {
        model.insert("api".into(), json!(api));
    }
    if base_url != provider.base_url {
        model.insert("baseUrl".into(), json!(base_url));
    }
    if let Some(reasoning) = source.get("reasoning").filter(|r| r.is_boolean()) {
        model.insert("reasoning".into(), reasoning.clone());
    }
    if !input.is_empty() {
        model.insert("input".into(), Value::Array(input));
    }
    let limit = source.get("limit");
    if let Some(context) = positive(limit.and_then(|l| l.get("context"))) {
        model.insert("contextWindow".into(), context.clone());
    }
    if let Some(output) = positive(limit.and_then(|l| l.get("output"))) {
        model.insert("maxTokens".into(), output.clone());
    }
    Some(Value::Object(model))
}

fn convert_headers(headers: Option<&Value>) -> Option<Map<String, Value>> {
    let Some(headers) = headers.and_then(Value::as_object) else {
        return Some(Map::new());
    };
    let mut converted = Map::new();
    for (name, value) in headers {
        let Some(value) = value.as_str() else {
            continue;
        };
        converted.insert(name.clone(), json!(config_value(value)?));
    }
    Some(converted)
}

/// A provider the stage carries, with the key it found for it.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedProvider {
    pub id: String,
    pub npm: String,
    pub config: Value,
    pub key: Option<ProviderKey>,
}

impl PlannedProvider {
    /// The model ids the provider config declares.
    pub fn model_ids(&self) -> Vec<String> {
        self.config
            .get("models")
            .and_then(Value::as_array)
            .map(|models| {
                models
                    .iter()
                    .filter_map(|m| m.get("id").and_then(Value::as_str).map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default()
    }
}

fn convert_provider(
    id: &str,
    entry: &Value,
    builtin: &BTreeSet<String>,
    auth: &Map<String, Value>,
    notices: &mut Vec<String>,
) -> Option<PlannedProvider> {
    let catalog = entry.get("catalog") == Some(&Value::Bool(true));
    let kind = if catalog {
        "catalog provider"
    } else {
        "custom provider"
    };
    if builtin.contains(id) {
        notices.push(format!("NOTICE opencode: provider {id} is one omo already serves; its opencode overrides (baseURL, models) are not imported - set them in models.json by hand if you need them"));
        return None;
    }
    let entry = entry.as_object()?;
    let npm = text(entry.get("npm")).unwrap_or(DEFAULT_NPM);
    let Some(api) = api_by_npm(npm) else {
        notices.push(format!("NOTICE opencode: {kind} {id} uses npm package {npm}, which omo has no API adapter for; not imported"));
        return None;
    };
    let options_base_url = text(entry.get("options").and_then(|o| o.get("baseURL")));
    let base_url_raw = options_base_url.or_else(|| text(entry.get("api")));
    let Some(fixed) = fixed_url(base_url_raw) else {
        notices.push(format!("NOTICE opencode: {kind} {id} has no fixed baseURL; not imported - omo needs a literal endpoint URL"));
        return None;
    };
    let Some(base_url) = engine_base_url(api, fixed) else {
        notices.push(format!("NOTICE opencode: {kind} {id} baseURL {fixed} does not end in /v1, which omo's Anthropic client needs; not imported"));
        return None;
    };
    let Some(headers) = convert_headers(entry.get("options").and_then(|o| o.get("headers"))) else {
        notices.push(format!("NOTICE opencode: {kind} {id} has a header with a {{file:...}} or {{env:...}} placeholder omo cannot express; not imported"));
        return None;
    };
    let context = ProviderContext {
        id,
        api,
        base_url_raw,
        base_url: &base_url,
        options_base_url,
    };
    let models: Vec<Value> = entry
        .get("models")
        .and_then(Value::as_object)
        .map(|models| {
            models
                .iter()
                .filter_map(|(model_id, model)| convert_model(&context, model_id, model, notices))
                .collect()
        })
        .unwrap_or_default();
    if models.is_empty() {
        notices.push(format!(
            "NOTICE opencode: {kind} {id} declares no models omo can use; not imported"
        ));
        return None;
    }
    let mut config = Map::new();
    if let Some(name) = text(entry.get("name")) {
        config.insert("name".into(), json!(name));
    }
    config.insert("baseUrl".into(), json!(base_url));
    config.insert("api".into(), json!(api));
    if !headers.is_empty() {
        config.insert("headers".into(), Value::Object(headers));
    }
    config.insert("models".into(), Value::Array(models));
    Some(PlannedProvider {
        id: id.to_owned(),
        npm: npm.to_owned(),
        config: Value::Object(config),
        key: convert_key(id, entry, auth, notices),
    })
}

// OpenCode's own copy of the models.dev catalog.
fn read_opencode_catalog(
    runtime: &SetupRuntime,
    notices: &mut Vec<String>,
) -> Option<Map<String, Value>> {
    let home = Path::new(&runtime.home);
    let path = runtime.env_value("OPENCODE_MODELS_PATH").map_or_else(
        || {
            runtime
                .env_value("XDG_CACHE_HOME")
                .map_or_else(|| home.join(".cache"), PathBuf::from)
                .join("opencode")
                .join("models.json")
        },
        PathBuf::from,
    );
    let raw = std::fs::read_to_string(&path).ok()?;
    match serde_json::from_str::<Value>(&raw) {
        Ok(Value::Object(map)) => Some(map),
        Ok(_) => None,
        Err(_) => {
            notices.push("WARN opencode: could not parse its models.dev catalog cache; catalog providers were not imported".into());
            None
        }
    }
}

// OpenCode extends a catalog model field by field: nested objects are merged.
fn overlay(base: Option<&Value>, own: &Value) -> Value {
    match (base.and_then(Value::as_object), own.as_object()) {
        (Some(base), Some(own)) => {
            let mut merged = base.clone();
            for (key, value) in own {
                let next = if value.is_object() && base.get(key).is_some_and(Value::is_object) {
                    overlay(base.get(key), value)
                } else {
                    value.clone()
                };
                merged.insert(key.clone(), next);
            }
            Value::Object(merged)
        }
        _ => own.clone(),
    }
}

// A catalog provider in the shape of a `provider.<id>` block. Deprecated models stay behind.
fn catalog_entry(entry: Option<&Value>) -> Option<Map<String, Value>> {
    let entry = entry?.as_object()?;
    let models: Map<String, Value> = entry
        .get("models")?
        .as_object()?
        .iter()
        .filter(|(_, model)| {
            model.is_object() && model.get("status").and_then(Value::as_str) != Some("deprecated")
        })
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let mut out = Map::new();
    for key in ["name", "npm", "api", "env"] {
        if let Some(value) = entry.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    out.insert("models".into(), Value::Object(models));
    out.insert("catalog".into(), Value::Bool(true));
    Some(out)
}

fn with_catalog(
    config: Map<String, Value>,
    auth: &Map<String, Value>,
    builtin: &BTreeSet<String>,
    runtime: &SetupRuntime,
    notices: &mut Vec<String>,
) -> Map<String, Value> {
    let keyed = auth
        .iter()
        .filter(|(_, e)| e.get("type").and_then(Value::as_str) == Some("api"))
        .map(|(id, _)| id.clone());
    let mut ids: Vec<String> = Vec::new();
    for id in keyed.chain(config.keys().cloned()) {
        if !builtin.contains(&id) && !ids.contains(&id) {
            ids.push(id);
        }
    }
    if ids.is_empty() {
        return config;
    }
    let own_endpoint = |id: &str| {
        config.get(id).is_some_and(|c| {
            text(c.get("options").and_then(|o| o.get("baseURL"))).is_some()
                || text(c.get("api")).is_some()
        })
    };
    let warn = ids.iter().any(|id| !own_endpoint(id));
    let mut discarded = Vec::new();
    let catalog = read_opencode_catalog(runtime, if warn { notices } else { &mut discarded });
    let Some(catalog) = catalog else {
        let missing: Vec<&str> = ids
            .iter()
            .filter(|id| !config.contains_key(*id))
            .map(String::as_str)
            .collect();
        if !missing.is_empty() {
            notices.push(format!("NOTICE opencode: no models.dev catalog cache found, so {} could not be described - if opencode serves them, run opencode once, then omo setup again", missing.join(", ")));
        }
        return config;
    };
    let mut merged = config.clone();
    for id in &ids {
        let Some(base) = catalog_entry(catalog.get(id)) else {
            continue;
        };
        let own = config
            .get(id)
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut models = base
            .get("models")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        if let Some(own_models) = own.get("models").and_then(Value::as_object) {
            for (model_id, model) in own_models {
                let next = overlay(models.get(model_id), model);
                models.insert(model_id.clone(), next);
            }
        }
        let mut entry = base;
        for (key, value) in own {
            entry.insert(key, value);
        }
        entry.insert("models".into(), Value::Object(models));
        merged.insert(id.clone(), Value::Object(entry));
    }
    merged
}

/// `planOpencodeProviders`' result.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OpencodeProviders {
    pub providers: Vec<PlannedProvider>,
    pub skipped: Vec<String>,
    pub notices: Vec<String>,
}

/// Every OpenCode custom or catalog provider the engine can serve, converted; everything else as a notice.
pub fn plan_opencode_providers(runtime: &SetupRuntime) -> OpencodeProviders {
    let mut notices = Vec::new();
    let config = read_opencode_section(
        &opencode_config_sources(runtime).files,
        "provider",
        "custom providers",
        &mut notices,
    );
    let builtin = builtin_provider_ids();
    // With no provider block, a bad auth.json is the credential stage's to report.
    let mut discarded = Vec::new();
    let auth = read_opencode_auth(
        runtime,
        if config.is_empty() {
            &mut discarded
        } else {
            &mut notices
        },
    );
    let declared = with_catalog(config, &auth, &builtin, runtime, &mut notices);
    if declared.is_empty() {
        return OpencodeProviders {
            notices,
            ..OpencodeProviders::default()
        };
    }
    let mut providers = Vec::new();
    let mut skipped = Vec::new();
    for (id, entry) in &declared {
        match convert_provider(id, entry, &builtin, &auth, &mut notices) {
            Some(provider) => providers.push(provider),
            None => skipped.push(id.clone()),
        }
    }
    providers.sort_by(|a, b| a.id.cmp(&b.id));
    skipped.sort();
    OpencodeProviders {
        providers,
        skipped,
        notices,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }

    fn runtime(home: &Path) -> SetupRuntime {
        SetupRuntime::new(home.to_string_lossy(), BTreeMap::new())
    }

    #[test]
    fn a_custom_provider_converts_with_its_key_and_models() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        write(
            &home.join(".config/opencode/opencode.json"),
            r#"{"provider":{
                "myco":{"name":"My Co","options":{"baseURL":"https://example.invalid/v1","headers":{"X":"{env:TOK}"}},"models":{"m1":{"name":"M1","id":"real-m1","limit":{"context":1000,"output":0},"modalities":{"input":["text","audio"]}}}},
                "anth":{"npm":"@ai-sdk/anthropic","options":{"baseURL":"https://a.invalid/v1","apiKey":"{env:AK}"},"models":{"c":{}}},
                "weird":{"npm":"@ai-sdk/unknown","models":{"x":{}}},
                "nourl":{"models":{"x":{}}},
                "openai":{"options":{"baseURL":"https://x"}}
            }}"#,
        );
        write(
            &home.join(".local/share/opencode/auth.json"),
            r#"{"myco":{"type":"api","key":"S$1"}}"#,
        );
        let plan = plan_opencode_providers(&runtime(home));
        let ids: Vec<&str> = plan.providers.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, vec!["anth", "myco"]);
        assert_eq!(plan.skipped, vec!["nourl", "openai", "weird"]);
        let myco = &plan.providers[1];
        assert_eq!(
            myco.config,
            json!({"name": "My Co", "baseUrl": "https://example.invalid/v1", "api": "openai-completions", "headers": {"X": "${TOK}"},
                   "models": [{"id": "m1", "upstreamModelId": "real-m1", "name": "M1", "input": ["text"], "contextWindow": 1000}]})
        );
        assert_eq!(
            myco.key,
            Some(ProviderKey {
                key: "S$$1".into(),
                source: "opencode auth.json".into()
            })
        );
        let anth = &plan.providers[0];
        assert_eq!(anth.config["baseUrl"], json!("https://a.invalid"));
        assert_eq!(
            anth.key,
            Some(ProviderKey {
                key: "${AK}".into(),
                source: "opencode config apiKey".into()
            })
        );
        assert!(
            plan.notices
                .iter()
                .any(|n| n.contains("provider openai is one omo already serves"))
        );
    }

    #[test]
    fn a_keyed_catalog_provider_is_described_from_the_models_dev_cache() {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path();
        write(
            &home.join(".local/share/opencode/auth.json"),
            r#"{"zen":{"type":"api","key":"K"}}"#,
        );
        let without = plan_opencode_providers(&runtime(home));
        assert!(without.providers.is_empty());
        assert!(
            without
                .notices
                .iter()
                .any(|n| n.contains("no models.dev catalog cache found, so zen"))
        );
        write(
            &home.join(".cache/opencode/models.json"),
            r#"{"zen":{"name":"Zen","api":"https://zen.invalid/v1","env":["ZEN_KEY"],"models":{"a":{"limit":{"context":5}},"old":{"status":"deprecated"}}}}"#,
        );
        let plan = plan_opencode_providers(&runtime(home));
        assert_eq!(plan.providers.len(), 1);
        assert_eq!(plan.providers[0].model_ids(), vec!["a"]);
        assert_eq!(
            plan.providers[0].key.as_ref().map(|k| k.key.as_str()),
            Some("K")
        );
    }
}
