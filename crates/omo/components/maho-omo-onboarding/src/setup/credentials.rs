//! The credential stage of `omo setup` (upstream `omo-native/bin/lib/setup-credentials.js`): API keys
//! from opencode's auth.json and the oh-my-pi / gajae-code agent databases, classified against the
//! engine's `<agentDir>/auth.json`. `plan*` only reads; [`apply_credentials`] is the one writer.

use super::auth_store::{
    AuthStore, KeyAddition, literal_config_value, read_auth_store, write_auth_store,
};
use super::guidance::{CredentialGuidance, credential_guidance};
use super::provider_map::{ProviderMap, provider_map};
use super::sqlite_rows::{open_read_only, read_row, read_rows};
use super::{SetupRuntime, list, sorted_unique};
use serde_json::Value;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

pub const API_KEY_TYPE_ACCEPTLIST: [&str; 1] = ["api_key"];
const SQLITE_STORES: [(&str, &str, i64); 2] = [("oh-my-pi", ".omp", 7), ("gajae-code", ".gjc", 4)];

/// One key a source holds: mapped onto an omo provider, or `unmapped` (no key kept).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub provider: String,
    pub key: Option<String>,
    pub source: String,
    pub unmapped: bool,
}

/// A key the stage adds, and where it came from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialAddition {
    pub provider: String,
    pub key: String,
    pub source: String,
}

#[derive(Default)]
struct Sources {
    candidates: Vec<Candidate>,
    oauth: Vec<String>,
    notices: Vec<String>,
}

fn target_provider(provider: &str, map: &ProviderMap) -> Option<String> {
    if map.is_builtin(provider) {
        return Some(provider.to_owned());
    }
    map.providers.get(provider).cloned()
}

fn candidate(provider: &str, key: String, source: &str, map: &ProviderMap) -> Candidate {
    match target_provider(provider, map) {
        Some(target) => Candidate {
            provider: target,
            key: Some(key),
            source: source.to_owned(),
            unmapped: false,
        },
        None => Candidate {
            provider: provider.to_owned(),
            key: None,
            source: source.to_owned(),
            unmapped: true,
        },
    }
}

fn read_opencode(path: &Path, map: &ProviderMap, plan: &mut Sources) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(entries)) => {
            for (provider, entry) in entries {
                let Some(entry) = entry.as_object() else {
                    continue;
                };
                match entry.get("type").and_then(Value::as_str) {
                    Some("oauth") => plan.oauth.push(provider),
                    Some("api") => {
                        if let Some(key) = entry.get("key").and_then(Value::as_str) {
                            plan.candidates.push(candidate(
                                &provider,
                                literal_config_value(key),
                                "opencode",
                                map,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(_) => {}
        Err(error) => plan
            .notices
            .push(format!("WARN opencode: could not parse auth.json: {error}")),
    }
}

fn read_sqlite_store(id: &str, path: &Path, expected: i64, map: &ProviderMap, plan: &mut Sources) {
    if !path.exists() {
        return;
    }
    let read = (|| -> Result<(), String> {
        let database = open_read_only(path)?;
        let version = read_row(
            &database,
            &["version"],
            "SELECT version FROM auth_schema_version",
        )?
        .and_then(|row| row.get("version").cloned());
        if version.as_ref().and_then(Value::as_i64) != Some(expected) {
            plan.notices.push(format!(
                "NOTICE {id}: auth schema version {} is unknown; credentials not imported",
                super::js_string(version.as_ref())
            ));
            return Ok(());
        }
        let rows = read_rows(
            &database,
            &["provider", "credential_type", "data"],
            "SELECT provider, credential_type, data FROM auth_credentials WHERE disabled_cause IS NULL ORDER BY id ASC",
        )?;
        for row in rows {
            let field = |key: &str| row.get(key).and_then(Value::as_str).unwrap_or_default();
            let (provider, kind) = (field("provider"), field("credential_type"));
            if kind == "oauth" {
                plan.oauth.push(provider.to_owned());
                continue;
            }
            if !API_KEY_TYPE_ACCEPTLIST.contains(&kind) {
                continue;
            }
            match serde_json::from_str::<Value>(field("data")) {
                Ok(data) => {
                    if let Some(key) = data.get("key").and_then(Value::as_str) {
                        plan.candidates
                            .push(candidate(provider, key.to_owned(), id, map));
                    }
                }
                Err(_) => plan.notices.push(format!(
                    "WARN {id}: ignored malformed {kind} row for {provider}"
                )),
            }
        }
        Ok(())
    })();
    if let Err(error) = read {
        plan.notices
            .push(format!("WARN {id}: could not inspect agent.db: {error}"));
    }
}

/// `$XDG_DATA_HOME/opencode/auth.json`, else `~/.local/share/opencode/auth.json`.
pub fn opencode_auth_path(runtime: &SetupRuntime) -> PathBuf {
    runtime
        .env_value("XDG_DATA_HOME")
        .map_or_else(
            || Path::new(&runtime.home).join(".local").join("share"),
            PathBuf::from,
        )
        .join("opencode")
        .join("auth.json")
}

/// The opencode providers signed in with OAuth; the model-choice stage carries their models to the
/// provider the credential plan tells the user to `/login` to.
pub fn opencode_oauth_providers(runtime: &SetupRuntime) -> HashSet<String> {
    let mut plan = Sources::default();
    read_opencode(&opencode_auth_path(runtime), provider_map(), &mut plan);
    plan.oauth.into_iter().collect()
}

fn read_sources(runtime: &SetupRuntime, map: &ProviderMap) -> Sources {
    let mut plan = Sources::default();
    read_opencode(&opencode_auth_path(runtime), map, &mut plan);
    for (id, directory, version) in SQLITE_STORES {
        let path = Path::new(&runtime.home)
            .join(directory)
            .join("agent")
            .join("agent.db");
        read_sqlite_store(id, &path, version, map, &mut plan);
    }
    plan
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CredentialResult {
    pub additions: Vec<CredentialAddition>,
    pub skipped_existing: Vec<String>,
    pub skipped_oauth: Vec<String>,
    pub skipped_unmapped: Vec<String>,
}

// An unmapped key whose provider the custom-provider stage carries over is imported there, with it.
fn classify(sources: &Sources, existing: &AuthStore, custom: &HashSet<String>) -> CredentialResult {
    let mut result = CredentialResult::default();
    let mut skipped_existing = Vec::new();
    let mut skipped_unmapped = Vec::new();
    let mut reserved: BTreeSet<String> = existing.entries.keys().cloned().collect();
    for item in &sources.candidates {
        if item.unmapped {
            if !custom.contains(&item.provider) {
                skipped_unmapped.push(item.provider.clone());
            }
        } else if reserved.contains(&item.provider) {
            skipped_existing.push(item.provider.clone());
        } else {
            reserved.insert(item.provider.clone());
            result.additions.push(CredentialAddition {
                provider: item.provider.clone(),
                key: item.key.clone().unwrap_or_default(),
                source: item.source.clone(),
            });
        }
    }
    result.skipped_existing = sorted_unique(skipped_existing);
    result.skipped_oauth = sorted_unique(sources.oauth.iter().cloned());
    result.skipped_unmapped = sorted_unique(skipped_unmapped);
    result
}

/// What the stage would write into `target`, and what it tells the user about the rest.
#[derive(Clone, Debug)]
pub struct CredentialPlan {
    pub target: PathBuf,
    pub current: AuthStore,
    pub notices: Vec<String>,
    pub malformed: bool,
    pub result: CredentialResult,
    pub guidance: CredentialGuidance,
    pub pending: usize,
    pub found: usize,
}

/// Read-only.
pub fn plan_credentials(
    runtime: &SetupRuntime,
    target: &Path,
    custom_provider_ids: &HashSet<String>,
) -> CredentialPlan {
    let map = provider_map();
    let sources = read_sources(runtime, map);
    let current = read_auth_store(target);
    let found = sources.candidates.len() + sources.oauth.len();
    if current.malformed {
        let mut notices = sources.notices;
        notices.push("WARN senpi: malformed auth.json; credentials were not imported".into());
        return CredentialPlan {
            target: target.to_path_buf(),
            current,
            notices,
            malformed: true,
            result: CredentialResult::default(),
            guidance: CredentialGuidance {
                logins: Vec::new(),
                unmapped: Vec::new(),
            },
            pending: 0,
            found,
        };
    }
    let result = classify(&sources, &current, custom_provider_ids);
    let guidance = credential_guidance(
        &result.skipped_oauth,
        &result.skipped_unmapped,
        map,
        &current.entries,
    );
    CredentialPlan {
        target: target.to_path_buf(),
        pending: result.additions.len(),
        current,
        notices: sources.notices,
        malformed: false,
        result,
        guidance,
        found,
    }
}

/// Routes credential additions through the engine's locked store instead of the direct writer.
pub type WriteAuthFn<'a> = &'a mut dyn FnMut(&str, &[KeyAddition]) -> Result<(), String>;

/// The stage's one writer.
///
/// # Errors
/// The store or the file write failed.
pub fn apply_credentials(
    plan: &CredentialPlan,
    write_auth: Option<WriteAuthFn<'_>>,
) -> Result<(), String> {
    let additions: Vec<KeyAddition> = plan
        .result
        .additions
        .iter()
        .map(|item| KeyAddition {
            provider: item.provider.clone(),
            key: item.key.clone(),
        })
        .collect();
    if let Some(write) = write_auth {
        return write(&plan.target.to_string_lossy(), &additions);
    }
    write_auth_store(&plan.target, &plan.current, &additions).map_err(|e| e.to_string())
}

pub fn credential_plan_lines(plan: &CredentialPlan) -> Vec<String> {
    if plan.malformed {
        return Vec::new();
    }
    let r = &plan.result;
    vec![
        list(
            "planned-add",
            &r.additions
                .iter()
                .map(|i| i.provider.clone())
                .collect::<Vec<_>>(),
        ),
        list("skipped-existing", &r.skipped_existing),
        list("skipped-oauth", &r.skipped_oauth),
        list("skipped-unmapped", &r.skipped_unmapped),
    ]
}

pub fn credential_counts(plan: &CredentialPlan) -> Vec<String> {
    if plan.malformed {
        return Vec::new();
    }
    let r = &plan.result;
    vec![
        format!("imported: {}", r.additions.len()),
        format!("skipped-existing: {}", r.skipped_existing.len()),
        format!("skipped-oauth: {}", r.skipped_oauth.len()),
        format!("skipped-unmapped: {}", r.skipped_unmapped.len()),
    ]
}

pub fn credential_question(plan: &CredentialPlan) -> String {
    format!(
        "Import API credentials for {} into {}? [y/N] ",
        plan.result
            .additions
            .iter()
            .map(|i| i.provider.as_str())
            .collect::<Vec<_>>()
            .join(", "),
        plan.target.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::detect::tests::create_database;
    use serde_json::json;
    use std::collections::BTreeMap;

    struct Fixture {
        _dir: tempfile::TempDir,
        runtime: SetupRuntime,
        target: PathBuf,
        xdg: PathBuf,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        let xdg = dir.path().join("xdg");
        std::fs::create_dir_all(&home).expect("mkdir");
        let mut env = BTreeMap::new();
        env.insert("XDG_DATA_HOME".into(), xdg.to_string_lossy().into_owned());
        Fixture {
            target: dir.path().join("agent/auth.json"),
            runtime: SetupRuntime::new(home.to_string_lossy(), env),
            xdg,
            _dir: dir,
        }
    }

    fn write(path: &Path, value: &Value) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, value.to_string()).expect("write");
    }

    fn read(path: &Path) -> Value {
        serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("json")
    }

    /// Upstream "opencode api oauth mapped and gateway entries": every usable api id imports.
    #[test]
    fn every_usable_api_id_imports_and_the_rest_is_reported() {
        let f = fixture();
        write(
            &f.xdg.join("opencode/auth.json"),
            &json!({
                "google": {"type": "api", "key": "S0"},
                "anthropic-api": {"type": "api", "key": "S1"},
                "xai": {"type": "oauth", "access": "S2"},
                "opencode": {"type": "api", "key": "S3"},
                "opencode-go": {"type": "api", "key": "S4"},
                "zai-coding-plan": {"type": "api", "key": "S5"},
                "unknown-gateway": {"type": "api", "key": "S6"},
            }),
        );
        let plan = plan_credentials(&f.runtime, &f.target, &HashSet::new());
        apply_credentials(&plan, None).expect("apply");
        assert_eq!(
            read(&f.target),
            json!({
                "google": {"type": "api_key", "key": "S0"},
                "anthropic": {"type": "api_key", "key": "S1"},
                "opencode": {"type": "api_key", "key": "S3"},
                "opencode-go": {"type": "api_key", "key": "S4"},
                "zai": {"type": "api_key", "key": "S5"},
            })
        );
        let counts = credential_counts(&plan);
        assert!(counts.contains(&"imported: 5".to_owned()), "{counts:?}");
        assert!(counts.contains(&"skipped-oauth: 1".to_owned()));
        assert!(counts.contains(&"skipped-unmapped: 1".to_owned()));
        assert_eq!(plan.result.skipped_unmapped, vec!["unknown-gateway"]);
        assert_eq!(plan.found, 7);
    }

    /// Upstream "existing senpi provider": its entry stays structurally byte-identical, and a second
    /// run imports nothing.
    #[test]
    fn existing_entries_are_kept_and_a_rerun_is_idempotent() {
        let f = fixture();
        let existing =
            json!({"type": "api_key", "key": "EXISTING", "metadata": {"order": [3, 2, 1]}});
        write(&f.target, &json!({"google": existing}));
        write(
            &f.xdg.join("opencode/auth.json"),
            &json!({"google": {"type": "api", "key": "S0"}, "openai": {"type": "api", "key": "!S1$x"}}),
        );
        let plan = plan_credentials(&f.runtime, &f.target, &HashSet::new());
        assert_eq!(plan.result.skipped_existing, vec!["google"]);
        apply_credentials(&plan, None).expect("apply");
        let stored = read(&f.target);
        assert_eq!(stored["google"].to_string(), existing.to_string());
        assert_eq!(stored["openai"]["key"], json!("$!S1$$x"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&f.target)
                .expect("meta")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let second = plan_credentials(&f.runtime, &f.target, &HashSet::new());
        assert_eq!(second.pending, 0);
        assert!(credential_counts(&second).contains(&"imported: 0".to_owned()));
    }

    #[test]
    fn database_keys_import_unknown_schemas_are_noticed_and_malformed_targets_block() {
        let f = fixture();
        let home = PathBuf::from(&f.runtime.home);
        create_database(
            &home.join(".omp/agent/agent.db"),
            7,
            &[
                ("openai", "api_key", r#"{"key":"DB-KEY"}"#, None),
                ("google", "api_key", r#"{"key":"OFF"}"#, Some("expired")),
                ("xai", "oauth", "{}", None),
                ("mistral", "api_key", "not json", None),
            ],
        );
        create_database(&home.join(".gjc/agent/agent.db"), 99, &[]);
        let plan = plan_credentials(&f.runtime, &f.target, &HashSet::new());
        assert_eq!(
            plan.result.additions,
            vec![CredentialAddition {
                provider: "openai".into(),
                key: "DB-KEY".into(),
                source: "oh-my-pi".into()
            }]
        );
        assert_eq!(plan.result.skipped_oauth, vec!["xai"]);
        assert!(
            plan.notices
                .contains(&"WARN oh-my-pi: ignored malformed api_key row for mistral".to_owned())
        );
        assert!(
            plan.notices.contains(
                &"NOTICE gajae-code: auth schema version 99 is unknown; credentials not imported"
                    .to_owned()
            )
        );
        std::fs::create_dir_all(f.target.parent().expect("parent")).expect("mkdir");
        std::fs::write(&f.target, "{oops").expect("write");
        let blocked = plan_credentials(&f.runtime, &f.target, &HashSet::new());
        assert!(blocked.malformed && blocked.pending == 0);
        assert!(blocked.notices.contains(
            &"WARN senpi: malformed auth.json; credentials were not imported".to_owned()
        ));
    }

    #[test]
    fn a_custom_provider_key_is_left_to_the_provider_stage() {
        let f = fixture();
        write(
            &f.xdg.join("opencode/auth.json"),
            &json!({"myco": {"type": "api", "key": "S"}}),
        );
        let custom: HashSet<String> = ["myco".to_owned()].into_iter().collect();
        let plan = plan_credentials(&f.runtime, &f.target, &custom);
        assert!(plan.result.skipped_unmapped.is_empty());
        assert!(opencode_oauth_providers(&f.runtime).is_empty());
    }
}
