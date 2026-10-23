//! Port of `feedback/collect-host.ts`: the host-side report sections.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::collect::{blank_config_secrets, engine_log_text, transcript_text};
use crate::draft::{CollectedSection, FeedbackSectionId};

/// The live session as the report sees it. `entries` are raw session entry objects.
#[derive(Clone, Debug, Default)]
pub struct HostSessionView {
    pub cwd: Option<String>,
    pub session_id: Option<String>,
    pub model: Option<String>,
    pub entries: Vec<Value>,
}

#[derive(Clone, Debug)]
pub struct HostFacts {
    pub env: BTreeMap<String, String>,
    pub omo_version: String,
    pub maho_version: String,
    pub channel: String,
    pub now: DateTime<Utc>,
    pub session: Option<HostSessionView>,
}

#[derive(Clone, Debug, Default)]
pub struct HostCollection {
    pub sections: Vec<CollectedSection>,
    pub install_hash: Option<String>,
    pub session_hash: Option<String>,
}

fn locale(env: &BTreeMap<String, String>) -> String {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|key| env.get(*key))
        .map(|value| value.split('.').next().unwrap_or("").replace('_', "-"))
        .find(|value| !value.is_empty() && value != "C" && value != "POSIX")
        .unwrap_or_else(|| "unknown".into())
}

fn os_release() -> String {
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|text| text.trim().to_string())
        .unwrap_or_else(|_| "unknown".into())
}

fn time_zone(env: &BTreeMap<String, String>) -> String {
    env.get("TZ")
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| Local::now().format("UTC%:z").to_string())
}

fn is_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn read_install_id(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let trimmed = text.trim();
    is_hex_64(trimmed).then(|| trimmed.to_string())
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    fs::write(path, contents)
}

/// Port of `getOmoNativeAttribution(...).install_hash`: sha256 of the install id (`OMO_NATIVE_INSTALL_ID`
/// when it is 64 hex chars, else the persisted `<state dir>/install-id`, created on first use).
pub fn native_install_hash(env: &BTreeMap<String, String>, state_dir: &Path) -> Option<String> {
    let from_env = env
        .get("OMO_NATIVE_INSTALL_ID")
        .map(|value| value.trim().to_string());
    let install_id = match from_env.filter(|value| is_hex_64(value)) {
        Some(id) => id,
        None => {
            let path = state_dir.join("install-id");
            match read_install_id(&path) {
                Some(id) => id,
                None => {
                    let fresh = format!(
                        "{}{}",
                        uuid::Uuid::new_v4().simple(),
                        uuid::Uuid::new_v4().simple()
                    );
                    fs::create_dir_all(state_dir).ok()?;
                    match write_private(&path, &format!("{fresh}\n")) {
                        Ok(()) => fresh,
                        Err(_) => read_install_id(&path)?,
                    }
                }
            }
        }
    };
    Some(format!("{:x}", Sha256::digest(install_id.as_bytes())))
}

fn telemetry_allowed(env: &BTreeMap<String, String>, config: &Value) -> bool {
    let telemetry_env: HashMap<String, String> = env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    !telemetry_core::should_disable_telemetry(&telemetry_core::ShouldDisableTelemetryInput {
        env: Some(&telemetry_env),
        global_env_prefix: None,
        product_env_prefix: "OMO_NATIVE",
    }) && omo_config_core::is_omo_telemetry_enabled(config)
}

fn home_dir(env: &BTreeMap<String, String>) -> PathBuf {
    env.get("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("/"))
}

pub fn collect_host_sections(facts: &HostFacts) -> HostCollection {
    let cwd_override = facts
        .session
        .as_ref()
        .and_then(|session| session.cwd.clone());
    let loaded =
        maho_omo_config_resolution::load_senpi_omo_config(omo_config_core::LoadOmoConfigOptions {
            cwd: cwd_override,
            env: Some(facts.env.clone()),
            ..Default::default()
        });
    let hashes_allowed =
        loaded.diagnostics.is_empty() && telemetry_allowed(&facts.env, &loaded.config);

    let telemetry_env: HashMap<String, String> = facts
        .env
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    let state_dir = maho_omo_telemetry::product_identity::get_omo_native_state_dir(&telemetry_env);
    let install_hash = if hashes_allowed {
        native_install_hash(&facts.env, &state_dir)
    } else {
        None
    };
    let session_hash = facts
        .session
        .as_ref()
        .and_then(|session| session.session_id.as_deref())
        .filter(|_| hashes_allowed)
        .and_then(|id| maho_omo_telemetry::product_identity::hash_session_id(id, &state_dir).ok());

    let device = gethostname::gethostname().to_string_lossy().into_owned();
    let mut identity = vec![format!("device    {device}")];
    if let Some(hash) = &install_hash {
        identity.push(format!("install   {hash}"));
    }
    if let Some(hash) = &session_hash {
        identity.push(format!("session   {hash}"));
    }
    let environment = [
        format!("omo {} · maho {}", facts.omo_version, facts.maho_version),
        format!(
            "{} {} · {} · {} · {} channel",
            std::env::consts::OS,
            os_release(),
            std::env::consts::ARCH,
            locale(&facts.env),
            facts.channel
        ),
        format!(
            "reported {} ({})",
            facts.now.to_rfc3339_opts(SecondsFormat::Millis, true),
            time_zone(&facts.env)
        ),
    ]
    .join("\n");

    let config_text = serde_json::to_string_pretty(&blank_config_secrets(&loaded.config, false))
        .unwrap_or_else(|_| "{}".into());
    let mut sections = vec![
        CollectedSection::verbatim(FeedbackSectionId::Identity, identity.join("\n")),
        CollectedSection::new(FeedbackSectionId::Environment, environment),
        CollectedSection::new(FeedbackSectionId::Config, config_text),
    ];
    let agent_home = maho_omo_agent_home::resolve_agent_home(
        &facts.env,
        &home_dir(&facts.env),
        &std::env::current_dir().unwrap_or_default(),
        Path::exists,
    );
    let logs = engine_log_text(&agent_home, facts.now);
    if !logs.is_empty() {
        sections.push(CollectedSection::new(FeedbackSectionId::Logs, logs));
    }
    if let Some(session) = &facts.session {
        let header = [
            format!(
                "session {} · model {}",
                session.session_id.as_deref().unwrap_or("unknown"),
                session.model.as_deref().unwrap_or("unknown")
            ),
            format!("cwd {}", session.cwd.as_deref().unwrap_or("unknown")),
        ]
        .join("\n");
        sections.push(CollectedSection::new(
            FeedbackSectionId::Session,
            format!("{header}\n{}", transcript_text(&session.entries)),
        ));
    }
    HostCollection {
        sections,
        install_hash,
        session_hash,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_hash_prefers_env_id_and_persists_generated_one() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut env = BTreeMap::new();
        env.insert("OMO_NATIVE_INSTALL_ID".to_string(), "a".repeat(64));
        let from_env = native_install_hash(&env, dir.path()).expect("hash");
        assert_eq!(
            from_env,
            format!("{:x}", Sha256::digest("a".repeat(64).as_bytes()))
        );
        assert!(!dir.path().join("install-id").exists());

        let first = native_install_hash(&BTreeMap::new(), dir.path()).expect("hash");
        let second = native_install_hash(&BTreeMap::new(), dir.path()).expect("hash");
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        assert!(dir.path().join("install-id").is_file());
    }
}
