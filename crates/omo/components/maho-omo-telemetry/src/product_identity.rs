pub use crate::model_vocabulary::{KNOWN_MODELS, KNOWN_PROVIDERS, ALL_KNOWN_MODEL_IDS};
use std::{collections::HashMap,io::Write,path::{Path,PathBuf},sync::{Mutex,OnceLock}};
#[cfg(unix)]
fn private_file(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600) }
#[cfg(not(unix))]
fn private_file(options: &mut std::fs::OpenOptions) -> &mut std::fs::OpenOptions { options }
use sha2::{Digest,Sha256};
pub fn create_omo_native_product_config() -> telemetry_core::TelemetryProductConfig {
    let mut config=crate::index::create_senpi_telemetry_product_config();
    config.cache_dir_name="omo-native".into();
    config.default_api_key="phc_r6UYQzNZcGYSzKw4PxCiVrZepGqV3dw9qcvcKtRNUWAn".into();
    config.event_name="daily_active".into();
    config.product_name="omo-native".into();
    config
}
pub const BUILTIN_SKILL_NAMES: &[&str] = &["adversarial-plan", "ast-grep", "browser", "coding-agent-sessions", "dag-library", "data-scientist", "debugging", "frontend", "git-master", "give-me-tips", "init-deep", "lsp-setup", "onboarding", "programming", "refactor", "remove-ai-slops", "review-work", "ulw-execute", "ultimate-browsing", "ultrawork", "ulw-loop", "ulw-plan", "ulw-research", "visual-qa", "visualize"];
pub use crate::event_schemas::{BUILTIN_CATEGORY_NAMES, EVENT_PROPERTY_ALLOWLISTS, OMO_NATIVE_COMMON_PROPERTY_SCHEMAS, OMO_NATIVE_EVENT_SCHEMAS, OMO_NATIVE_PROPERTY_ALLOWLISTS, omo_native_event_schemas, builtin_category_names, curated_agents};
/// Shipped v5.1.38 builtin skill vocabulary.
pub fn pinned_skill_names()->Vec<String> {BUILTIN_SKILL_NAMES.iter().map(|name|(*name).to_string()).collect()}
/// The selected-source skill vocabulary of the staged root the telemetry component consumes:
/// `<skills_root>/../manifest.json` `skills.names`, written by `tools/package-native.mjs` for the
/// selected delivery. `None` when the manifest is absent or malformed (dev tree), never a panic.
pub fn staged_skill_names(skills_root:&Path)->Option<Vec<String>> {
    let manifest=skills_root.parent()?.join("manifest.json");
    let raw=std::fs::read_to_string(manifest).ok()?;
    let value:serde_json::Value=serde_json::from_str(&raw).ok()?;
    let names=value.get("skills")?.get("names")?.as_array()?;
    let names:Vec<String>=names.iter().filter_map(serde_json::Value::as_str).map(str::to_owned).collect();
    (!names.is_empty()).then_some(names)
}
/// The runtime skill vocabulary: the selected manifest's names when present, else the pinned set.
pub fn selected_skill_names(skills_root:&Path)->Vec<String> {staged_skill_names(skills_root).unwrap_or_else(pinned_skill_names)}
static FALLBACK_SALTS:OnceLock<Mutex<HashMap<PathBuf,[u8;32]>>>=OnceLock::new();
pub fn get_omo_native_state_dir(env:&telemetry_core::TelemetryEnv)->PathBuf {
    crate::index::get_senpi_agent_dir(env).join("omo-senpi").join("omo-native")
}
pub fn hash_session_id(raw:&str,state_dir:&Path)->std::io::Result<String> {
    let path=state_dir.join("session-id-salt");
    let read=||std::fs::read(&path).ok().and_then(|b|<[u8;32]>::try_from(b).ok());
    let salt=if let Some(salt)=read() {salt} else {
        let mut salt=[0;32];getrandom::fill(&mut salt).map_err(std::io::Error::other)?;
        let write=|exclusive:bool|->std::io::Result<()> {std::fs::create_dir_all(state_dir)?;let mut options=std::fs::OpenOptions::new();options.write(true).create(!exclusive).create_new(exclusive).truncate(!exclusive);let mut file=private_file(&mut options).open(&path)?;file.write_all(&salt)};
        if write(true).is_ok() {FALLBACK_SALTS.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&path);salt}
        else if let Some(existing)=read() {existing}
        else if write(false).is_ok() {FALLBACK_SALTS.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&path);salt}
        else {*FALLBACK_SALTS.get_or_init(Default::default).lock().unwrap_or_else(std::sync::PoisonError::into_inner).entry(path).or_insert(salt)}
    };
    let mut hash=Sha256::new();hash.update(salt);hash.update(raw.as_bytes());Ok(format!("{:x}",hash.finalize()))
}
pub fn mask_provider_and_model(provider: &str, model: &str) -> (String, String) {
    let known_provider=KNOWN_PROVIDERS.contains(&provider);
    (if known_provider {provider} else {"custom"}.into(), if ALL_KNOWN_MODEL_IDS.contains(model) {model} else {"custom"}.into())
}

/// Schema version shared by every native client.
pub const OMO_NATIVE_SCHEMA_VERSION: u32 = 4;

/// The first UTC day `machine_hash` no longer ships (exclusive; the last day it ships is 2026-12-14).
pub const MACHINE_HASH_SUNSET: &str = "2026-12-15";
const MACHINE_HASH_SUNSET_ISO: &str = "2026-12-15T00:00:00.000Z";
const INSTALL_ID_FILE_NAME: &str = "install-id";

fn is_install_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn user_home(env: &telemetry_core::TelemetryEnv) -> PathBuf {
    let key = if cfg!(target_os = "windows") {
        "USERPROFILE"
    } else {
        "HOME"
    };
    env.get(key)
        .map(|home| home.trim())
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var(key).ok().map(PathBuf::from))
        .unwrap_or_default()
}

/// One anonymous id per user, whatever agent home a process resolves. The id lives at the path OmO Desktop
/// uses, `<user home>/.omo/agent/omo-senpi/omo-native/install-id`, so an agent-dir override (an
/// orchestrator's isolated agent dir per run) or a pre-unification flat `~/.omo` layout no longer mints a
/// new person.
pub fn get_omo_native_install_id_path(env: &telemetry_core::TelemetryEnv) -> PathBuf {
    user_home(env)
        .join(".omo")
        .join("agent")
        .join("omo-senpi")
        .join("omo-native")
        .join(INSTALL_ID_FILE_NAME)
}

fn without_agent_dir_overrides(
    env: &telemetry_core::TelemetryEnv,
) -> std::collections::BTreeMap<String, String> {
    env.iter()
        .filter(|(name, _)| !maho_omo_agent_home::AGENT_DIR_ENV_NAMES.contains(&name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect()
}

fn read_install_id(path: &Path) -> Option<String> {
    let value = std::fs::read_to_string(path).ok()?;
    let value = value.trim();
    is_install_id(value).then(|| value.to_string())
}

fn write_install_id(path: &Path, value: &str) -> bool {
    if let Some(parent) = path.parent() {
        let _ignored = std::fs::create_dir_all(parent);
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    let mut file = match private_file(&mut options).open(path) {
        Ok(file) => file,
        Err(_) => return false,
    };
    file.write_all(format!("{value}\n").as_bytes()).is_ok()
}

fn random_install_id() -> std::io::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes).map_err(std::io::Error::other)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn read_or_create_install_id(path: &Path) -> String {
    if let Some(persisted) = read_install_id(path) {
        return persisted;
    }
    let generated = random_install_id().unwrap_or_else(|_| "0".repeat(64));
    if write_install_id(path, &generated) {
        return generated;
    }
    // A concurrent writer (the Desktop, or another CLI process) may have created it first; its id wins.
    read_install_id(path).unwrap_or(generated)
}

/// The legacy id is the one the user's own install wrote, so an orchestrator's agent-dir override is
/// ignored here: otherwise its first post-upgrade run would mint the permanent id instead of adopting the
/// real one.
fn resolve_user_install_id(env: &telemetry_core::TelemetryEnv) -> String {
    let canonical = get_omo_native_install_id_path(env);
    let home = user_home(env);
    let cwd = std::env::current_dir().unwrap_or_default();
    let legacy = maho_omo_agent_home::resolve_agent_home(
        &without_agent_dir_overrides(env),
        &home,
        &cwd,
        std::path::Path::exists,
    )
    .join("omo-senpi")
    .join("omo-native")
    .join(INSTALL_ID_FILE_NAME);
    if legacy == canonical {
        return read_or_create_install_id(&canonical);
    }
    if let Some(shared) = read_install_id(&canonical) {
        return shared;
    }
    let Some(adopted) = read_install_id(&legacy) else {
        return read_or_create_install_id(&canonical);
    };
    let _exclusive = write_install_id(&canonical, &adopted);
    read_install_id(&canonical).unwrap_or(adopted)
}

fn is_ci_env(value: Option<&String>) -> bool {
    let normalized = value.map(|value| value.trim().to_lowercase()).unwrap_or_default();
    !normalized.is_empty() && normalized != "0" && normalized != "false"
}

/// Where the event came from: the standalone CLI, or a runtime embedded in OmO Desktop.
pub struct OmoNativeAttributionInput<'a> {
    pub env: Option<&'a telemetry_core::TelemetryEnv>,
    /// Injected by tests; defaults to the runtime's resolved agent home.
    pub state_dir: Option<&'a Path>,
    /// Decides whether `machine_hash` is still inside its transition window.
    pub now: Option<chrono::DateTime<chrono::Utc>>,
    pub os_provider: Option<&'a dyn telemetry_core::TelemetryOsProvider>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OmoNativeAttribution {
    pub surface: &'static str,
    pub install_hash: String,
    pub ci: bool,
    pub machine_hash: Option<String>,
}

/// Shared attribution for every omo-native event. The install id is a random 64-hex id stored next to the
/// session-id salt, so the CLI and the Desktop-bundled runtime on one machine converge on one id without
/// deriving anything from the machine itself. A valid `OMO_NATIVE_INSTALL_ID` env override wins, because a
/// Desktop host pins a remote (SSH/WSL) runtime to ITS local installation id.
pub fn get_omo_native_attribution(input: &OmoNativeAttributionInput<'_>) -> OmoNativeAttribution {
    let empty = telemetry_core::TelemetryEnv::new();
    let env = input.env.unwrap_or(&empty);
    let surface = match env.get("OMO_NATIVE_SURFACE").map(|value| value.trim()) {
        Some("desktop") => "desktop",
        _ => "cli",
    };
    let env_install_id = env
        .get("OMO_NATIVE_INSTALL_ID")
        .map(|value| value.trim())
        .unwrap_or_default();
    let install_id = if is_install_id(env_install_id) {
        env_install_id.to_string()
    } else if let Some(state_dir) = input.state_dir {
        read_or_create_install_id(&state_dir.join(INSTALL_ID_FILE_NAME))
    } else {
        resolve_user_install_id(env)
    };
    let now = input.now.unwrap_or_else(chrono::Utc::now);
    let sunset = chrono::DateTime::parse_from_rfc3339(MACHINE_HASH_SUNSET_ISO)
        .map(|sunset| sunset.with_timezone(&chrono::Utc))
        .unwrap_or_else(|_| now);
    let machine_hash = if now < sunset {
        let os: &dyn telemetry_core::TelemetryOsProvider = input
            .os_provider
            .unwrap_or(telemetry_core::get_default_telemetry_os_provider());
        Some(telemetry_core::get_telemetry_distinct_id(
            &create_omo_native_product_config().machine_id_prefix,
            os,
        ))
    } else {
        None
    };
    let mut hash = Sha256::new();
    hash.update(install_id.as_bytes());
    OmoNativeAttribution {
        surface,
        install_hash: format!("{:x}", hash.finalize()),
        ci: is_ci_env(env.get("CI")),
        machine_hash,
    }
}

/// Attach the shared attribution to a product config; the fixed identity keys still win downstream.
pub fn with_omo_native_attribution(
    product: telemetry_core::TelemetryProductConfig,
    input: &OmoNativeAttributionInput<'_>,
) -> telemetry_core::TelemetryProductConfig {
    let attribution = get_omo_native_attribution(input);
    let mut properties = product.additional_properties.clone().unwrap_or_default();
    properties.insert("surface".into(), serde_json::json!(attribution.surface));
    properties.insert("install_hash".into(), serde_json::json!(attribution.install_hash));
    properties.insert("ci".into(), serde_json::json!(attribution.ci));
    if let Some(machine_hash) = attribution.machine_hash {
        properties.insert("machine_hash".into(), serde_json::json!(machine_hash));
    }
    telemetry_core::TelemetryProductConfig {
        additional_properties: Some(properties),
        ..product
    }
}
#[cfg(test)]
mod identity_tests {
    use super::*;
    #[test]
    fn package_version_uses_build_metadata() {
        assert_eq!(create_omo_native_product_config().package_version, env!("CARGO_PKG_VERSION"));
    }
    #[test]
    fn shipped_models_survive_private_gateways() {
        for &(provider, models) in KNOWN_MODELS {
            assert!(KNOWN_PROVIDERS.contains(&provider));
            for &model in models {
                assert!(ALL_KNOWN_MODEL_IDS.contains(model));
                assert_eq!(mask_provider_and_model(provider, model), (provider.into(), model.into()));
                assert_eq!(mask_provider_and_model("private-gateway", model), ("custom".into(), model.into()));
            }
        }
        let anthropic = KNOWN_MODELS.iter().find(|(provider, _)| *provider == "anthropic").expect("anthropic vocabulary").1;
        let subscription = KNOWN_MODELS.iter().find(|(provider, _)| *provider == "anthropic-subscription").expect("subscription vocabulary").1;
        assert_eq!(subscription, anthropic);
        assert_eq!(mask_provider_and_model("deepseek", "claude-opus-5"), ("deepseek".into(), "claude-opus-5".into()));
        for (provider, model) in [("private-gateway", "my-finetune"), ("openrouter", "acme-internal/claude-opus-5-ft"), ("private-gateway", "")] {
            assert_eq!(mask_provider_and_model(provider, model), ("custom".into(), "custom".into()));
        }
        assert_eq!(mask_provider_and_model("anthropic", "my-finetune"), ("anthropic".into(), "custom".into()));
    }
    #[test]
    fn task_insight_schemas_preserve_measurements_and_vocabulary() {
        let schemas = omo_native_event_schemas();
        let delegation = &schemas["delegation_completed"];
        assert_eq!(delegation["status"]["values"], serde_json::json!(["completed", "error", "cancelled", "interrupted", "lost"]));
        assert_eq!(delegation["start_reason"]["values"], serde_json::json!(["initial_spawn", "runtime_fallback", "session_resume", "dag_retry", "revive_after_completed", "revive_after_error", "revive_after_cancelled", "revive_after_interrupted", "revive_after_lost", "unknown"]));
        assert_eq!(delegation["owner_kind"]["values"], serde_json::json!(["plain_child", "dag_node", "team_member", "unknown"]));
        assert_eq!(delegation["background_mode"]["values"], serde_json::json!(["foreground", "background", "promoted", "unknown"]));
        assert_eq!(delegation["execution_mode"]["values"], serde_json::json!(["in-process", "process"]));
        assert_eq!(delegation["reasoning_effort"]["values"], serde_json::json!(["off", "minimal", "low", "medium", "high", "xhigh", "max", "other", "none"]));
        assert_eq!(delegation["model_source"]["values"], serde_json::json!(["category", "explicit", "agent", "none"]));
        for key in ["token_status", "stats_status"] {
            assert_eq!(delegation[key]["values"], serde_json::json!(["complete", "partial", "unavailable"]));
        }
        assert_eq!(delegation["cost_status"]["values"], serde_json::json!(["reported", "partial", "unavailable", "invalid"]));
        assert_eq!(delegation["duration_status"]["values"], serde_json::json!(["monotonic", "wall_clock", "unavailable"]));
        assert_eq!(delegation["category"]["values"], serde_json::json!(builtin_category_names().into_iter().chain(["custom", "none"]).collect::<Vec<_>>()));
        assert_eq!(delegation["agent_type"]["values"], serde_json::json!(curated_agents().into_iter().chain(["custom", "none"]).collect::<Vec<_>>()));
        for key in ["task_seq", "run_epoch", "config_generation", "duration_ms", "turns", "tool_calls"] {
            assert_eq!(delegation[key], serde_json::json!({"type":"number"}));
        }
        assert_eq!(schemas["category_config"]["source"]["values"], schemas["session_started"]["reason"]["values"]);
        assert_eq!(schemas["category_config"]["config_generation"], serde_json::json!({"type":"number"}));
        assert_eq!(schemas["session_started"]["timezone"], serde_json::json!({"type":"string"}));
        assert_eq!(schemas["turn_completed"]["sample_rate"], serde_json::json!({"type":"number"}));
    }
    #[test] fn native_product_identity() {let c=create_omo_native_product_config();assert_eq!(c.platform,"omo-senpi");assert_eq!(c.machine_id_prefix,"omo-senpi:");assert_eq!(c.product_env_prefix,"OMO_SENPI");assert_eq!(c.event_name,"daily_active");assert_eq!(c.cache_dir_name,"omo-native");assert!(!c.default_api_key.is_empty());}
    #[test] fn explicit_agent_state_dir() {let e=telemetry_core::TelemetryEnv::from([("SENPI_CODING_AGENT_DIR".into(),"/fixture/agent".into())]);let expected=crate::index::get_senpi_agent_dir(&e).join("omo-senpi").join("omo-native");assert_eq!(get_omo_native_state_dir(&e),expected);}
    #[test] fn deleted_salt_recreated() {let t=tempfile::tempdir().unwrap();hash_session_id("s",t.path()).unwrap();std::fs::remove_file(t.path().join("session-id-salt")).unwrap();hash_session_id("s",t.path()).unwrap();assert!(t.path().join("session-id-salt").is_file());}
    #[test] fn histogram_privacy_bound() {let widest=std::iter::repeat_n(crate::wave_assembler::MAX_TRACKED_CALLS.to_string(),8).collect::<Vec<_>>().join(":");assert_eq!(widest.len(),39);assert!(widest.len()<=64);}
    #[test] fn static_allowlists_unique() {assert!(!BUILTIN_SKILL_NAMES.is_empty());for (_,keys) in EVENT_PROPERTY_ALLOWLISTS {let unique:std::collections::HashSet<_>=keys.iter().collect();assert_eq!(unique.len(),keys.len());}}
    #[test] fn imported_task_names_exact() {assert_eq!(builtin_category_names(),BUILTIN_CATEGORY_NAMES);assert_eq!(curated_agents(),senpi_task::agents::curated_readonly_agent_names());}
    #[test] fn schema_keys_match_capture_allowlists() {let schemas=omo_native_event_schemas();for (name,keys) in EVENT_PROPERTY_ALLOWLISTS {let mut actual=schemas[*name].as_object().unwrap().keys().map(String::as_str).collect::<Vec<_>>();let mut expected=keys.to_vec();actual.sort();expected.sort();assert_eq!(actual,expected);}assert_eq!(schemas["parallelism_summary"].as_object().unwrap().len(),crate::parallelism_schema::PARALLELISM_SUMMARY_SCHEMA.len());}
    #[test] fn schemas_preserve_closed_vocabularies_and_types() {let s=omo_native_event_schemas();assert_eq!(s["session_started"]["cpu_count"]["type"],"number");assert_eq!(s["prompt_submitted"]["keyword_any"]["type"],"boolean");assert_eq!(s["feature_used"]["feature"]["values"],serde_json::json!(["goal_tool","team_create","memory_tool"]));assert_eq!(s["parallelism_summary"]["schema_kind"]["values"],serde_json::json!(["parallelism_v1","parallelism_v2"]));assert!(s["delegation_started"]["name"]["values"].as_array().unwrap().contains(&serde_json::json!(builtin_category_names()[0])));assert!(!s.to_string().contains("prompt_text"));}
    #[test] fn salted_hash_stable_private() {let t=tempfile::tempdir().unwrap();let a=hash_session_id("private-session",t.path()).unwrap();assert_eq!(a,hash_session_id("private-session",t.path()).unwrap());assert_ne!(a,hash_session_id("other-session",t.path()).unwrap());assert_eq!(a.len(),64);assert!(!a.contains("private-session"));assert_eq!(std::fs::read(t.path().join("session-id-salt")).unwrap().len(),32);}
    #[cfg(unix)]
    #[test] fn salt_file_private() {use std::os::unix::fs::PermissionsExt;let t=tempfile::tempdir().unwrap();hash_session_id("s",t.path()).unwrap();assert_eq!(std::fs::metadata(t.path().join("session-id-salt")).unwrap().permissions().mode()&0o777,0o600);}
    #[test] fn invalid_salt_repaired() {let t=tempfile::tempdir().unwrap();std::fs::write(t.path().join("session-id-salt"),"invalid").unwrap();hash_session_id("s",t.path()).unwrap();assert_eq!(std::fs::read(t.path().join("session-id-salt")).unwrap().len(),32);}
    #[test] fn fallback_salt_stable() {let t=tempfile::tempdir().unwrap();let blocked=t.path().join("file");std::fs::write(&blocked,"").unwrap();assert_eq!(hash_session_id("s",&blocked).unwrap(),hash_session_id("s",&blocked).unwrap());}
    #[test] fn selected_skill_names_follow_the_staged_manifest() {
        let t=tempfile::tempdir().unwrap();let skills=t.path().join("skills");std::fs::create_dir(&skills).unwrap();
        assert_eq!(selected_skill_names(&skills),pinned_skill_names());
        std::fs::write(t.path().join("manifest.json"),serde_json::json!({"skills":{"source":"latest","names":["browser","ulw-execute"]}}).to_string()).unwrap();
        assert_eq!(selected_skill_names(&skills),vec!["browser".to_string(),"ulw-execute".to_string()]);
        assert!(!BUILTIN_SKILL_NAMES.contains(&"start-work"));
        assert!(BUILTIN_SKILL_NAMES.contains(&"browser"));
    }
    #[test] fn pure_schema_stays_pinned() {
        let pinned=omo_native_event_schemas()["skill_loaded"]["skill_name"]["values"].as_array().unwrap().clone();
        assert!(pinned.contains(&serde_json::json!("browser")));
        assert!(!pinned.contains(&serde_json::json!("start-work")));
    }
}
