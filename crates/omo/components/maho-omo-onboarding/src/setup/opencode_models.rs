//! Reads the model choices an OpenCode user made - the opencode config's `model`, `small_model` and
//! `agent.<name>` overrides, and the OpenCode edition's omo routing (`categories`, `agents`) - and
//! converts each into what the native harness reads (upstream
//! `omo-native/bin/lib/setup-opencode-models.js`). Provider ids go through provider-map.json, and every
//! provider/model is checked against the engine's model list; what does not resolve is returned with
//! its reason, never converted. Read-only.

use super::SetupRuntime;
use super::engine_models::ModelRegistry;
use super::opencode_assets::{opencode_config_sources, read_opencode_config};
use super::provider_map::provider_map;
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

// The OpenCode edition's legacy plugin files, highest precedence first.
const EDITION_FILES: [&str; 4] = [
    "oh-my-openagent.jsonc",
    "oh-my-openagent.json",
    "oh-my-opencode.jsonc",
    "oh-my-opencode.json",
];

/// The agents the native harness defines.
pub const NATIVE_AGENT_NAMES: [&str; 7] = [
    "explore",
    "librarian",
    "omo-native-code-reviewer",
    "omo-native-gate-reviewer",
    "omo-native-qa-executor",
    "plan-consultant",
    "plan-reviewer",
];

// OpenCode's own primary agents: the native main session has no per-mode agent.
const OPENCODE_PRIMARY_AGENTS: [&str; 2] = ["build", "plan"];
const REASONING: [&str; 9] = [
    "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto", "none",
];
const MODEL_KEYS: [&str; 3] = ["model", "models", "fallback_models"];

fn agent_alias(name: &str) -> Option<&'static str> {
    match name {
        "metis" => Some("plan-consultant"),
        "momus" => Some("plan-reviewer"),
        _ => None,
    }
}

fn category_alias(name: &str) -> Option<&'static str> {
    (name == "deep").then_some("deep-low")
}

fn record(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

// The first start of any current harness moves a legacy file to
// `~/.omo/migration-backup-<timestamp>-opencode-config/<path relative to home>`; the newest moved
// copy stands in for it.
fn edition_file(path: PathBuf, home: &Path, notices: &mut Vec<String>) -> PathBuf {
    if path.exists() {
        return path;
    }
    let Ok(in_home) = path.strip_prefix(home) else {
        return path;
    };
    let root = home.join(".omo");
    let Ok(entries) = std::fs::read_dir(&root) else {
        return path;
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|name| {
            name.starts_with("migration-backup-")
                && name.ends_with("-opencode-config")
                && name.len() > "migration-backup--opencode-config".len()
        })
        .collect();
    names.sort();
    names.reverse();
    let Some(moved) = names
        .into_iter()
        .map(|name| root.join(name).join(in_home))
        .find(|c| c.exists())
    else {
        return path;
    };
    notices.push(format!(
        "NOTICE opencode: {} was moved to {} by the omo config migration; its model routing is read from there",
        path.display(),
        moved.display()
    ));
    moved
}

/// The OpenCode edition's own routing (`agents`, `categories`).
pub struct EditionRouting {
    pub agents: Map<String, Value>,
    pub categories: Map<String, Value>,
    pub notices: Vec<String>,
}

pub fn read_edition_routing(runtime: &SetupRuntime) -> EditionRouting {
    let home = Path::new(&runtime.home);
    let mut notices = Vec::new();
    let global_dir = runtime
        .env_value("XDG_CONFIG_HOME")
        .map_or_else(|| home.join(".config"), PathBuf::from)
        .join("opencode");
    let explicit = runtime
        .env
        .get("OPENCODE_CONFIG_DIR")
        .map(|v| v.trim())
        .filter(|v| !v.is_empty());
    let directories = match explicit {
        Some(dir) => vec![
            std::path::absolute(dir).unwrap_or_else(|_| PathBuf::from(dir)),
            global_dir,
        ],
        None => vec![global_dir],
    };
    // read_opencode_config lets later files win, so the highest-precedence file is read last.
    let mut files: Vec<PathBuf> = directories
        .iter()
        .flat_map(|dir| EDITION_FILES.iter().map(move |name| dir.join(name)))
        .collect::<Vec<_>>()
        .into_iter()
        .map(|path| edition_file(path, home, &mut notices))
        .collect();
    files.reverse();
    let edition = read_opencode_config(&files, "model choices", &mut notices);
    EditionRouting {
        agents: record(edition.get("agents")),
        categories: record(edition.get("categories")),
        notices,
    }
}

/// The raw choices, before any translation.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RawModelChoices {
    pub model: Option<String>,
    pub small_model: Option<String>,
    pub opencode_agents: Map<String, Value>,
    pub categories: Map<String, Value>,
    pub agents: Map<String, Value>,
    pub notices: Vec<String>,
}

/// `opencode_block` is the `[opencode]` block of the user's omo.json[c]; an entry there wins over the
/// legacy files.
pub fn read_model_choices(
    runtime: &SetupRuntime,
    opencode_block: Option<&Value>,
) -> RawModelChoices {
    let mut notices = Vec::new();
    let opencode = read_opencode_config(
        &opencode_config_sources(runtime).files,
        "model choices",
        &mut notices,
    );
    let edition = read_edition_routing(runtime);
    let (categories, agents) = edition_choices(&edition, opencode_block);
    notices.extend(edition.notices);
    RawModelChoices {
        model: opencode
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        small_model: opencode
            .get("small_model")
            .and_then(Value::as_str)
            .map(str::to_owned),
        opencode_agents: record(opencode.get("agent")),
        categories,
        agents,
        notices,
    }
}

fn edition_choices(
    edition: &EditionRouting,
    opencode_block: Option<&Value>,
) -> (Map<String, Value>, Map<String, Value>) {
    let block = record(opencode_block);
    let mut categories = edition.categories.clone();
    categories.extend(record(block.get("categories")));
    let mut agents = edition.agents.clone();
    agents.extend(record(block.get("agents")));
    (categories, agents)
}

fn has_model_choice(entry: &Value) -> bool {
    entry
        .as_object()
        .is_some_and(|e| MODEL_KEYS.iter().any(|k| e.get(*k).is_some()))
}

pub fn has_model_choices(raw: &RawModelChoices) -> bool {
    raw.model.is_some()
        || raw.small_model.is_some()
        || [&raw.opencode_agents, &raw.categories, &raw.agents]
            .iter()
            .any(|entries| entries.values().any(has_model_choice))
}

struct Context<'a> {
    registry: Option<&'a ModelRegistry>,
    oauth_providers: &'a HashSet<String>,
}

fn engine_provider(provider: &str, model_id: &str, context: &Context<'_>) -> String {
    let map = provider_map();
    if context.oauth_providers.contains(provider)
        && let Some(login) = map.oauth_logins.get(provider)
        && context.registry.is_some_and(|r| {
            r.models
                .get(login)
                .is_some_and(|ids| ids.contains(model_id))
        })
    {
        return login.clone();
    }
    if map.is_builtin(provider) {
        return provider.to_owned();
    }
    map.providers
        .get(provider)
        .cloned()
        .unwrap_or_else(|| provider.to_owned())
}

// `provider/model`, optionally with a `:level` or legacy `(level)` reasoning suffix.
fn split_suffix(text: &str) -> (String, String) {
    if let Some(stripped) = text.strip_suffix(')')
        && let Some(open) = stripped.rfind('(')
    {
        let level = stripped[open + 1..].trim().to_lowercase();
        let head = stripped[..open].trim_end();
        if !stripped[open + 1..].contains(['(', ')'])
            && !head.is_empty()
            && REASONING.contains(&level.as_str())
        {
            return (head.to_owned(), format!(":{level}"));
        }
    }
    let Some(colon) = text.rfind(':') else {
        return (text.to_owned(), String::new());
    };
    let token = text[colon + 1..].to_lowercase();
    if colon == 0
        || !REASONING.contains(&token.as_str())
        || (token == "max" && !text[..colon].contains('/'))
    {
        return (text.to_owned(), String::new());
    }
    (text[..colon].to_owned(), format!(":{token}"))
}

/// A translated model reference, or the reason it could not be.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Translated {
    Ref {
        reference: String,
        provider: Option<String>,
        model_id: String,
    },
    Reason(String),
}

/// `translateModelRef`. Without a registry only the provider is checked, against the builtins.
pub fn translate_model_ref(
    raw: Option<&Value>,
    registry: Option<&ModelRegistry>,
    oauth_providers: &HashSet<String>,
) -> Translated {
    translate(
        raw,
        &Context {
            registry,
            oauth_providers,
        },
    )
}

fn translate(raw: Option<&Value>, context: &Context<'_>) -> Translated {
    let Some(raw) = raw.and_then(Value::as_str).filter(|r| !r.trim().is_empty()) else {
        return Translated::Reason("not a model string".into());
    };
    let (selector, suffix) = split_suffix(raw.trim());
    let Some(slash) = selector.find('/').filter(|s| *s > 0) else {
        // A bare id is matched against every provider by the native resolver.
        let served = context
            .registry
            .is_none_or(|r| r.models.values().any(|ids| ids.contains(&selector)));
        return if served {
            Translated::Ref {
                reference: format!("{selector}{suffix}"),
                provider: None,
                model_id: selector,
            }
        } else {
            Translated::Reason(format!("no omo provider serves a model named {selector}"))
        };
    };
    let source = &selector[..slash];
    let model_id = selector[slash + 1..].to_owned();
    let provider = engine_provider(source, &model_id, context);
    let Some(registry) = context.registry else {
        return if provider_map().is_builtin(&provider) {
            Translated::Ref {
                reference: format!("{provider}/{model_id}{suffix}"),
                provider: Some(provider),
                model_id,
            }
        } else {
            Translated::Reason(format!(
                "provider {source} is not one omo serves or was carried over"
            ))
        };
    };
    let Some(ids) = registry.models.get(&provider) else {
        return Translated::Reason(format!(
            "provider {source} is not one omo serves or was carried over"
        ));
    };
    if !ids.contains(&model_id) {
        return Translated::Reason(if registry.dynamic.contains(&provider) {
            format!(
                "{provider} lists its models only at runtime, so {model_id} could not be checked"
            )
        } else {
            format!("omo's {provider} provider has no model {model_id}")
        });
    }
    Translated::Ref {
        reference: format!("{provider}/{model_id}{suffix}"),
        provider: Some(provider),
        model_id,
    }
}

fn tuning(entry: &Map<String, Value>, out: &mut Map<String, Value>) {
    for key in ["reasoning", "reasoningEffort", "variant"] {
        if let Some(value) = entry
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            out.insert("reasoning".into(), json!(value));
            break;
        }
    }
    if let Some(t) = entry
        .get("temperature")
        .filter(|t| t.as_f64().is_some_and(|t| (0.0..=2.0).contains(&t)))
    {
        out.insert("temperature".into(), t.clone());
    }
}

/// A choice that was not converted, and (for an unmapped agent) the native key that would set it.
#[derive(Clone, Debug, PartialEq)]
pub struct Dropped {
    pub text: String,
    pub target: Option<DropTarget>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct DropTarget {
    pub section: &'static str,
    pub name: String,
    pub model: Option<String>,
}

fn convert_entry(
    label: &str,
    entry: &Map<String, Value>,
    context: &Context<'_>,
    dropped: &mut Vec<Dropped>,
    target: &DropTarget,
) -> Option<Value> {
    let mut take = |raw: Option<&Value>| -> Option<String> {
        match translate(raw, context) {
            Translated::Ref { reference, .. } => Some(reference),
            Translated::Reason(reason) => {
                let shown = super::js_string(raw);
                dropped.push(Dropped {
                    text: format!("{label} model {shown}: {reason}"),
                    target: Some(target.clone()),
                });
                None
            }
        }
    };
    let models_is_array = entry.get("models").is_some_and(Value::is_array);
    let folded = !models_is_array && entry.get("fallback_models").is_some();
    let chain: Vec<Value> = if let Some(models) = entry.get("models").and_then(Value::as_array) {
        models.clone()
    } else if folded {
        let mut chain: Vec<Value> = entry.get("model").cloned().into_iter().collect();
        match entry.get("fallback_models") {
            Some(Value::Array(items)) => chain.extend(items.iter().cloned()),
            Some(other) => chain.push(other.clone()),
            None => {}
        }
        chain
    } else {
        Vec::new()
    };
    let model = if !folded && entry.contains_key("model") {
        take(entry.get("model"))
    } else {
        None
    };
    let mut models = Vec::new();
    for item in &chain {
        match item {
            Value::String(_) => {
                if let Some(reference) = take(Some(item)) {
                    models.push(json!(reference));
                }
            }
            Value::Object(object) => {
                let Some(reference) = take(object.get("model")) else {
                    continue;
                };
                let mut out = Map::new();
                out.insert("model".into(), json!(reference));
                tuning(object, &mut out);
                models.push(Value::Object(out));
            }
            _ => {}
        }
    }
    if has_model_choice(&Value::Object(entry.clone())) && model.is_none() && models.is_empty() {
        return None;
    }
    let mut converted = Map::new();
    if let Some(model) = model {
        converted.insert("model".into(), json!(model));
    }
    if !models.is_empty() {
        converted.insert("models".into(), Value::Array(models));
    }
    tuning(entry, &mut converted);
    (!converted.is_empty()).then_some(Value::Object(converted))
}

/// A converted category or agent.
#[derive(Clone, Debug, PartialEq)]
pub struct NamedChoice {
    pub name: String,
    pub entry: Value,
    pub from: String,
}

fn first_model(entry: &Map<String, Value>, context: &Context<'_>) -> Option<String> {
    // JS `entry.model ?? ...`: a null model counts as absent.
    let raw = entry
        .get("model")
        .filter(|m| !m.is_null())
        .cloned()
        .or_else(|| {
            [entry.get("models"), entry.get("fallback_models")]
                .into_iter()
                .flatten()
                .flat_map(|v| match v {
                    Value::Array(items) => items.clone(),
                    other => vec![other.clone()],
                })
                .next()
        })?;
    let text = match &raw {
        Value::Object(o) => o.get("model").cloned()?,
        other => other.clone(),
    };
    let text_str = text.as_str()?.to_owned();
    match translate(Some(&text), context) {
        Translated::Ref { reference, .. } => Some(reference),
        Translated::Reason(_) => Some(text_str),
    }
}

fn convert_agents(
    raw: &RawModelChoices,
    context: &Context<'_>,
    dropped: &mut Vec<Dropped>,
) -> Vec<NamedChoice> {
    let mut agents: BTreeMap<String, (Value, String)> = BTreeMap::new();
    for (origin, entries) in [
        ("opencode agent", &raw.opencode_agents),
        ("agent", &raw.agents),
    ] {
        for (name, entry) in entries {
            if !has_model_choice(entry) {
                continue;
            }
            let target = agent_alias(name).unwrap_or(name.as_str()).to_owned();
            if target != *name && entries.get(&target).is_some_and(has_model_choice) {
                continue;
            }
            let Some(object) = entry.as_object() else {
                continue;
            };
            if !NATIVE_AGENT_NAMES.contains(&target.as_str()) {
                dropped.push(if OPENCODE_PRIMARY_AGENTS.contains(&name.as_str()) && origin == "opencode agent" {
                    Dropped { text: format!("{origin} {name}: the OpenCode primary agent; omo's main session runs the default model instead"), target: None }
                } else {
                    Dropped {
                        text: format!("{origin} {name}: omo has no agent of that name (its agents: {}); route that work through a category instead", NATIVE_AGENT_NAMES.join(", ")),
                        target: Some(DropTarget { section: "categories", name: "<category>".into(), model: first_model(object, context) }),
                    }
                });
                continue;
            }
            let drop_target = DropTarget {
                section: "agents",
                name: target.clone(),
                model: None,
            };
            if let Some(converted) = convert_entry(
                &format!("{origin} {name}"),
                object,
                context,
                dropped,
                &drop_target,
            ) {
                agents.insert(target, (converted, name.clone()));
            }
        }
    }
    agents
        .into_iter()
        .map(|(name, (entry, from))| NamedChoice { name, entry, from })
        .collect()
}

fn convert_categories(
    raw: &RawModelChoices,
    context: &Context<'_>,
    dropped: &mut Vec<Dropped>,
) -> Vec<NamedChoice> {
    let mut categories: BTreeMap<String, (Value, String)> = BTreeMap::new();
    for (name, entry) in &raw.categories {
        let Some(object) = entry.as_object() else {
            continue;
        };
        let target = category_alias(name).unwrap_or(name.as_str()).to_owned();
        if target != *name && raw.categories.contains_key(&target) {
            continue;
        }
        let drop_target = DropTarget {
            section: "categories",
            name: target.clone(),
            model: None,
        };
        if let Some(converted) = convert_entry(
            &format!("category {name}"),
            object,
            context,
            dropped,
            &drop_target,
        ) {
            categories.insert(target, (converted, name.clone()));
        }
    }
    categories
        .into_iter()
        .map(|(name, (entry, from))| NamedChoice { name, entry, from })
        .collect()
}

/// The converted default model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultModel {
    pub source: String,
    pub provider: String,
    pub model_id: String,
    pub reference: String,
}

fn convert_default(
    model: Option<&str>,
    context: &Context<'_>,
    dropped: &mut Vec<Dropped>,
) -> Option<DefaultModel> {
    let model = model?;
    match translate(Some(&json!(model)), context) {
        Translated::Reason(reason) => {
            dropped.push(Dropped {
                text: format!("default model {model}: {reason}"),
                target: None,
            });
            None
        }
        Translated::Ref { provider: None, .. } => {
            dropped.push(Dropped {
                text: format!(
                    "default model {model}: names no provider, and omo's default model needs one"
                ),
                target: None,
            });
            None
        }
        Translated::Ref {
            provider: Some(provider),
            model_id,
            ..
        } => Some(DefaultModel {
            source: model.to_owned(),
            reference: format!("{provider}/{model_id}"),
            provider,
            model_id,
        }),
    }
}

/// The raw choices converted against `registry`.
#[derive(Clone, Debug, PartialEq)]
pub struct ConvertedChoices {
    pub default_model: Option<DefaultModel>,
    pub categories: Vec<NamedChoice>,
    pub agents: Vec<NamedChoice>,
    pub dropped: Vec<String>,
}

pub fn convert_model_choices(
    raw: &RawModelChoices,
    registry: &ModelRegistry,
    oauth_providers: &HashSet<String>,
) -> ConvertedChoices {
    let context = Context {
        registry: Some(registry),
        oauth_providers,
    };
    let mut dropped = Vec::new();
    let default_model = convert_default(raw.model.as_deref(), &context, &mut dropped);
    if let Some(small) = &raw.small_model {
        dropped.push(Dropped {
            text: format!("small_model {small}: omo has no small-model setting"),
            target: None,
        });
    }
    let categories = convert_categories(raw, &context, &mut dropped);
    let agents = convert_agents(raw, &context, &mut dropped);
    ConvertedChoices {
        default_model,
        categories,
        agents,
        dropped: dropped.into_iter().map(|d| d.text).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn registry() -> ModelRegistry {
        let mut registry = ModelRegistry::default();
        let ids = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<BTreeSet<_>>();
        registry.models.insert("openai".into(), ids(&["gpt-5"]));
        registry
            .models
            .insert("chatgpt-subscription".into(), ids(&["gpt-5"]));
        registry
            .models
            .insert("opencode-go".into(), ids(&["glm-5.2"]));
        registry.models.insert("openrouter".into(), ids(&[]));
        registry.dynamic.insert("openrouter".into());
        registry
    }

    fn reference(raw: &str, oauth: &[&str]) -> Translated {
        let oauth: HashSet<String> = oauth.iter().map(|s| (*s).to_owned()).collect();
        translate_model_ref(Some(&json!(raw)), Some(&registry()), &oauth)
    }

    #[test]
    fn model_refs_resolve_suffixes_oauth_logins_and_report_reasons() {
        assert_eq!(
            reference("openai/gpt-5:high", &[]),
            Translated::Ref {
                reference: "openai/gpt-5:high".into(),
                provider: Some("openai".into()),
                model_id: "gpt-5".into()
            }
        );
        assert_eq!(
            reference("openai/gpt-5 (Max)", &["openai"]),
            Translated::Ref {
                reference: "chatgpt-subscription/gpt-5:max".into(),
                provider: Some("chatgpt-subscription".into()),
                model_id: "gpt-5".into()
            }
        );
        assert_eq!(
            reference("gpt-5", &[]),
            Translated::Ref {
                reference: "gpt-5".into(),
                provider: None,
                model_id: "gpt-5".into()
            }
        );
        assert_eq!(
            reference("nope", &[]),
            Translated::Reason("no omo provider serves a model named nope".into())
        );
        assert_eq!(
            reference("openai/gpt-9", &[]),
            Translated::Reason("omo's openai provider has no model gpt-9".into())
        );
        assert_eq!(
            reference("openrouter/x", &[]),
            Translated::Reason(
                "openrouter lists its models only at runtime, so x could not be checked".into()
            )
        );
        assert_eq!(
            reference("acme/x", &[]),
            Translated::Reason("provider acme is not one omo serves or was carried over".into())
        );
    }

    #[test]
    fn categories_agents_and_defaults_convert_with_aliases_and_drops() {
        let mut raw = RawModelChoices {
            model: Some("openai/gpt-5".into()),
            small_model: Some("openai/mini".into()),
            ..RawModelChoices::default()
        };
        raw.categories = json!({"deep": {"model": "opencode-go/glm-5.2", "temperature": 0.5}, "quick": {"model": "acme/x"}}).as_object().cloned().expect("obj");
        raw.agents = json!({"momus": {"model": "openai/gpt-5", "fallback_models": ["opencode-go/glm-5.2"]}, "sisyphus": {"model": "openai/gpt-5"}}).as_object().cloned().expect("obj");
        raw.opencode_agents = json!({"build": {"model": "openai/gpt-5"}})
            .as_object()
            .cloned()
            .expect("obj");
        let converted = convert_model_choices(&raw, &registry(), &HashSet::new());
        assert_eq!(
            converted
                .default_model
                .as_ref()
                .map(|d| d.reference.as_str()),
            Some("openai/gpt-5")
        );
        assert_eq!(
            converted.categories,
            vec![NamedChoice {
                name: "deep-low".into(),
                entry: json!({"model": "opencode-go/glm-5.2", "temperature": 0.5}),
                from: "deep".into()
            }]
        );
        assert_eq!(
            converted.agents,
            vec![NamedChoice {
                name: "plan-reviewer".into(),
                entry: json!({"models": ["openai/gpt-5", "opencode-go/glm-5.2"]}),
                from: "momus".into()
            }]
        );
        assert_eq!(
            converted.dropped,
            vec![
                "small_model openai/mini: omo has no small-model setting",
                "category quick model acme/x: provider acme is not one omo serves or was carried over",
                "opencode agent build: the OpenCode primary agent; omo's main session runs the default model instead",
                "agent sisyphus: omo has no agent of that name (its agents: explore, librarian, omo-native-code-reviewer, omo-native-gate-reviewer, omo-native-qa-executor, plan-consultant, plan-reviewer); route that work through a category instead",
            ]
        );
        assert!(has_model_choices(&raw));
        assert!(!has_model_choices(&RawModelChoices::default()));
    }
}
