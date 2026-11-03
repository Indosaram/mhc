//! The custom-provider stage of `omo setup` (upstream `omo-native/bin/lib/setup-providers-import.js`):
//! classifies what [`super::opencode_providers`] planned against the engine's `<agentDir>/models.json`
//! and `auth.json`. A provider id models.json already has, and a key auth.json already has, are never
//! overwritten. [`plan_providers`] only reads; [`apply_providers`] is the one writer.

use super::auth_store::{KeyAddition, read_auth_store, write_auth_store};
use super::credentials::WriteAuthFn;
use super::jsonc::parse_jsonc;
use super::list;
use super::opencode_providers::{OpencodeProviders, PlannedProvider};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};

/// The engine's models.json as the plan read it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModelsTarget {
    pub document: Map<String, Value>,
    pub bytes: Option<String>,
    pub malformed: bool,
}

// The engine strips comments before parsing models.json, so a commented file is read the same way.
// `providers` must be an object; a file where it is not is left alone.
fn read_models_target(path: &Path) -> ModelsTarget {
    let Ok(bytes) = std::fs::read_to_string(path) else {
        let mut document = Map::new();
        document.insert("providers".into(), Value::Object(Map::new()));
        return ModelsTarget {
            document,
            bytes: None,
            malformed: false,
        };
    };
    match parse_jsonc(&bytes) {
        Ok(Value::Object(mut document)) => {
            let providers = document
                .get("providers")
                .cloned()
                .unwrap_or_else(|| Value::Object(Map::new()));
            if !providers.is_object() {
                return ModelsTarget {
                    document: Map::new(),
                    bytes: Some(bytes),
                    malformed: true,
                };
            }
            document.insert("providers".into(), providers);
            ModelsTarget {
                document,
                bytes: Some(bytes),
                malformed: false,
            }
        }
        _ => ModelsTarget {
            document: Map::new(),
            bytes: Some(bytes),
            malformed: true,
        },
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProviderResult {
    pub added: Vec<PlannedProvider>,
    pub skipped_existing: Vec<String>,
    pub blocked: Vec<String>,
    pub keys: Vec<KeyAddition>,
    pub keys_existing: Vec<String>,
    pub keys_blocked: Vec<String>,
}

fn classify(
    plan: &OpencodeProviders,
    models: &ModelsTarget,
    auth: &super::auth_store::AuthStore,
) -> ProviderResult {
    let mut result = ProviderResult::default();
    let existing = models.document.get("providers").and_then(Value::as_object);
    for provider in &plan.providers {
        if models.malformed {
            result.blocked.push(provider.id.clone());
            continue;
        }
        if existing.is_some_and(|p| p.contains_key(&provider.id)) {
            result.skipped_existing.push(provider.id.clone());
            continue;
        }
        result.added.push(provider.clone());
        // A key only ever rides with the provider it belongs to.
        let Some(key) = &provider.key else { continue };
        if auth.malformed {
            result.keys_blocked.push(provider.id.clone());
        } else if auth.entries.contains_key(&provider.id) {
            result.keys_existing.push(provider.id.clone());
        } else {
            result.keys.push(KeyAddition {
                provider: provider.id.clone(),
                key: key.key.clone(),
            });
        }
    }
    result
}

/// Where the key of an added provider comes from, in the words the setup summary prints.
pub fn provider_key_source(provider: &PlannedProvider, result: &ProviderResult) -> String {
    if result.keys_existing.contains(&provider.id) {
        return "kept the existing auth.json key".into();
    }
    if result.keys_blocked.contains(&provider.id) {
        return "key not imported (malformed auth.json)".into();
    }
    match &provider.key {
        Some(key) => format!("key from {}", key.source),
        None => format!("no key found - /login {} inside omo", provider.id),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderPaths {
    pub models: PathBuf,
    pub auth: PathBuf,
}

/// The custom-provider stage's plan.
#[derive(Clone, Debug, PartialEq)]
pub struct ProviderPlan {
    pub paths: ProviderPaths,
    pub models: ModelsTarget,
    pub auth_bytes: Option<String>,
    pub notices: Vec<String>,
    pub result: ProviderResult,
    pub skipped: Vec<String>,
    pub present: bool,
    pub pending: usize,
}

pub fn provider_plan_lines(plan: &ProviderPlan) -> Vec<String> {
    vec![
        list(
            "planned-providers",
            &plan
                .result
                .added
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
        ),
        list("providers-skipped-existing", &plan.result.skipped_existing),
    ]
}

pub fn provider_counts(plan: &ProviderPlan) -> Vec<String> {
    vec![
        list(
            "providers-imported",
            &plan
                .result
                .added
                .iter()
                .map(|p| p.id.clone())
                .collect::<Vec<_>>(),
        ),
        format!("provider-keys-imported: {}", plan.result.keys.len()),
        format!(
            "providers-skipped-existing: {}",
            plan.result.skipped_existing.len()
        ),
    ]
}

pub fn provider_question(plan: &ProviderPlan) -> String {
    format!(
        "Import {} custom provider(s) into {} and {} API key(s) into {}? [y/N] ",
        plan.result.added.len(),
        plan.paths.models.display(),
        plan.result.keys.len(),
        plan.paths.auth.display()
    )
}

fn write_models(
    path: &Path,
    target: &ModelsTarget,
    added: &[PlannedProvider],
) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        super::create_private_dir(parent)?;
    }
    if target.bytes.is_some() {
        std::fs::copy(
            path,
            format!("{}.bak-{}", path.display(), super::timestamp()),
        )?;
    }
    let mut next = target.document.clone();
    let mut providers = next
        .get("providers")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for provider in added {
        providers.insert(provider.id.clone(), provider.config.clone());
    }
    next.insert("providers".into(), Value::Object(providers));
    // A models.json symlinked from a dotfiles checkout is written through the link.
    let destination = if target.bytes.is_some() {
        std::fs::canonicalize(path)?
    } else {
        path.to_path_buf()
    };
    // Provider headers can carry tokens, so this file gets the same 0600 the auth store does.
    let text = serde_json::to_string_pretty(&Value::Object(next)).map_err(std::io::Error::other)?;
    super::write_private(&destination, format!("{text}\n").as_bytes())
}

/// `source` is `planOpencodeProviders`' result, classified against the engine's files. Read-only.
pub fn plan_providers(source: &OpencodeProviders, agent_dir: &Path) -> ProviderPlan {
    let paths = ProviderPaths {
        models: agent_dir.join("models.json"),
        auth: agent_dir.join("auth.json"),
    };
    let models = read_models_target(&paths.models);
    let auth = read_auth_store(&paths.auth);
    let result = classify(source, &models, &auth);
    let mut notices = source.notices.clone();
    if !result.blocked.is_empty() {
        notices.push(format!(
            "WARN senpi: malformed models.json; these custom providers were not imported: {}",
            result.blocked.join(", ")
        ));
    }
    let pending = result.added.len();
    ProviderPlan {
        paths,
        models,
        auth_bytes: auth.bytes,
        notices,
        result,
        skipped: source.skipped.clone(),
        present: !source.providers.is_empty(),
        pending,
    }
}

/// The stage's one writer. Returns the keys it wrote (re-read against auth.json first: the
/// credential stage of the same run may have just written a key, which is kept).
///
/// # Errors
/// A filesystem step or the store failed.
pub fn apply_providers(
    plan: &ProviderPlan,
    write_auth: Option<WriteAuthFn<'_>>,
) -> Result<Vec<KeyAddition>, String> {
    write_models(&plan.paths.models, &plan.models, &plan.result.added)
        .map_err(|e| e.to_string())?;
    let auth = read_auth_store(&plan.paths.auth);
    let keys: Vec<KeyAddition> = if auth.malformed {
        Vec::new()
    } else {
        plan.result
            .keys
            .iter()
            .filter(|k| !auth.entries.contains_key(&k.provider))
            .cloned()
            .collect()
    };
    if keys.is_empty() {
        return Ok(keys);
    }
    match write_auth {
        Some(write) => write(&plan.paths.auth.to_string_lossy(), &keys)?,
        None => write_auth_store(&plan.paths.auth, &auth, &keys).map_err(|e| e.to_string())?,
    }
    Ok(keys)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::opencode_providers::ProviderKey;
    use serde_json::json;

    fn provider(id: &str, key: Option<&str>) -> PlannedProvider {
        PlannedProvider {
            id: id.into(),
            npm: "@ai-sdk/openai-compatible".into(),
            config: json!({"baseUrl": "https://x", "api": "openai-completions", "models": [{"id": "m"}]}),
            key: key.map(|k| ProviderKey {
                key: k.into(),
                source: "opencode auth.json".into(),
            }),
        }
    }

    #[test]
    fn new_providers_and_their_keys_land_and_existing_ones_are_kept() {
        let dir = tempfile::tempdir().expect("tempdir");
        let agent = dir.path().join("agent");
        std::fs::create_dir_all(&agent).expect("mkdir");
        std::fs::write(
            agent.join("models.json"),
            "{\n // mine\n \"providers\": {\"kept\": {\"x\": 1}},\n}",
        )
        .expect("write");
        std::fs::write(
            agent.join("auth.json"),
            r#"{"haskey":{"type":"api_key","key":"old"}}"#,
        )
        .expect("write");
        let source = OpencodeProviders {
            providers: vec![
                provider("kept", Some("k0")),
                provider("myco", Some("k1")),
                provider("haskey", Some("k2")),
                provider("nokey", None),
            ],
            skipped: vec!["skip".into()],
            notices: vec![],
        };
        let plan = plan_providers(&source, &agent);
        assert_eq!(plan.pending, 3);
        assert_eq!(plan.result.skipped_existing, vec!["kept"]);
        assert_eq!(plan.result.keys_existing, vec!["haskey"]);
        assert_eq!(
            provider_key_source(&plan.result.added[2], &plan.result),
            "no key found - /login nokey inside omo"
        );
        assert_eq!(
            provider_plan_lines(&plan),
            vec![
                "planned-providers: myco, haskey, nokey",
                "providers-skipped-existing: kept"
            ]
        );
        let written = apply_providers(&plan, None).expect("apply");
        assert_eq!(
            written,
            vec![KeyAddition {
                provider: "myco".into(),
                key: "k1".into()
            }]
        );
        let models: Value = serde_json::from_str(
            &std::fs::read_to_string(agent.join("models.json")).expect("read"),
        )
        .expect("json");
        assert_eq!(models["providers"]["kept"], json!({"x": 1}));
        assert_eq!(
            models["providers"]["myco"]["api"],
            json!("openai-completions")
        );
        let auth: Value =
            serde_json::from_str(&std::fs::read_to_string(agent.join("auth.json")).expect("read"))
                .expect("json");
        assert_eq!(auth["haskey"]["key"], json!("old"));
        assert_eq!(auth["myco"], json!({"type": "api_key", "key": "k1"}));
    }

    #[test]
    fn a_malformed_models_json_blocks_every_provider() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("models.json"), r#"{"providers": []}"#).expect("write");
        let source = OpencodeProviders {
            providers: vec![provider("myco", None)],
            ..OpencodeProviders::default()
        };
        let plan = plan_providers(&source, dir.path());
        assert_eq!(plan.result.blocked, vec!["myco"]);
        assert_eq!(plan.pending, 0);
        assert!(plan.notices[0].starts_with("WARN senpi: malformed models.json"));
    }
}
