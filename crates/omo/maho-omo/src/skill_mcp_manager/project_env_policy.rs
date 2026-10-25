pub use super::env_grants_store::declaration_digest as policy_hash;
use super::types::{Environment, SkillMcpClientInfo};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
pub const SAFE_MCP_ENV_NAMES: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "SHELL",
    "TERM",
    "TMPDIR",
    "TMP",
    "TEMP",
    "TZ",
    "XDG_CONFIG_HOME",
    "XDG_DATA_HOME",
    "XDG_CACHE_HOME",
    "LANG",
    "LANGUAGE",
    "LC_ALL",
    "LC_CTYPE",
    "LC_COLLATE",
    "LC_MESSAGES",
    "LC_MONETARY",
    "LC_NUMERIC",
    "LC_TIME",
    "LC_ADDRESS",
    "LC_IDENTIFICATION",
    "LC_MEASUREMENT",
    "LC_NAME",
    "LC_PAPER",
    "LC_TELEPHONE",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "HOMEDRIVE",
    "HOMEPATH",
    "USERNAME",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "PROGRAMFILES",
    "PROCESSOR_ARCHITECTURE",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "REQUESTS_CA_BUNDLE",
];
fn canonical_path(path: &str) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| {
        let path = Path::new(path);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir().unwrap_or_default().join(path)
        };
        let mut result = PathBuf::new();
        for part in path.components() {
            match part {
                std::path::Component::CurDir => {}
                std::path::Component::ParentDir => {
                    result.pop();
                }
                _ => result.push(part),
            }
        }
        result
    })
}
pub fn declaration_digest(info: &SkillMcpClientInfo, config: &Value) -> String {
    let provenance = info.provenance.as_ref();
    let project = provenance.and_then(|p| p.project_root.as_deref());
    policy_hash(
        &json!({"sourcePath":canonical_path(info.source_path.as_deref().or(provenance.map(|p|p.selected_root.as_str())).or(info.directory.as_deref()).unwrap_or(".")),"sourceKind":info.source_kind.as_deref().unwrap_or("frontmatter"),"projectRoot":canonical_path(project.or(info.directory.as_deref()).unwrap_or(".")),"serverName":info.server_name,"raw":config,"cwdBase":canonical_path(info.directory.as_deref().or(project).unwrap_or("."))}),
    )
}
pub fn uses_project_environment(info: &SkillMcpClientInfo) -> bool {
    if info
        .provenance
        .as_ref()
        .is_some_and(|p| p.kind == "project-auto")
    {
        return true;
    }
    if info.provenance.as_ref().is_some_and(|p| p.kind == "host")
        || info.scope.as_deref() == Some("host")
    {
        return !info.host_user_trusted;
    }
    match info.provenance.as_ref().map(|p| p.kind.as_str()) {
        Some("user" | "builtin") => false,
        _ => {
            if info.scope.as_deref() == Some("host") {
                !info.host_user_trusted
            } else {
                matches!(
                    info.scope.as_deref(),
                    Some("project" | "opencode-project" | "local")
                )
            }
        }
    }
}
fn package_exclusion(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.starts_with("NPM_CONFIG_")
        || name.starts_with("YARN_")
        || name.starts_with("PNPM_")
        || name == "NO_UPDATE_NOTIFIER"
}
pub fn create_clean_mcp_environment(custom: &Environment, ambient: &Environment) -> Environment {
    let excluded=regex::Regex::new(r"(?i)(^(ANTHROPIC_API_KEY|AWS_ACCESS_KEY_ID|AWS_SECRET_ACCESS_KEY|GOOGLE_APPLICATION_CREDENTIALS|GOOGLE_CLOUD_PROJECT|GITHUB_TOKEN|DATABASE_URL|OPENAI_API_KEY|KUBECONFIG)$|^(AZURE_|GCP_|FIREBASE_|HEROKU_|DOCKER_AUTH|VAULT_)|_(KEY|SECRET|TOKEN|PASSWORD|CREDENTIAL|CREDENTIALS)$)").expect("static pattern");
    let mut result: Environment = ambient
        .iter()
        .filter(|(name, _)| !package_exclusion(name) && !excluded.is_match(name))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    result.extend(custom.clone());
    result
}
pub struct ResolvedEnvironment {
    pub config: Value,
    pub env: Environment,
    pub withheld: Vec<String>,
}
pub fn resolve_project_environment(
    raw: &Value,
    granted_names: &[String],
    explicit: &Environment,
    ambient: &Environment,
    windows: bool,
) -> ResolvedEnvironment {
    let key = |name: &str| {
        if windows {
            name.to_uppercase()
        } else {
            name.to_owned()
        }
    };
    let mut values = BTreeMap::new();
    for (name, value) in ambient {
        let normalized = key(name);
        if !values.contains_key(&normalized) || name == &normalized {
            values.insert(normalized, (name.clone(), value.clone()));
        }
    }
    let grants: BTreeSet<_> = granted_names.iter().map(|n| key(n)).collect();
    let mut withheld = BTreeSet::new();
    let expression = regex::Regex::new(r"\$\{([^}:]+)(?::-([^}]*))?\}").expect("static pattern");
    let mut expand = |value: &str| {
        expression.replace_all(value,|capture: &regex::Captures<'_>|{
        let name=&capture[1];
        if !claude_code_compat_core::features::claude_code_mcp_loader::configure_allowed_env_vars::is_allowed_mcp_env_var(name)&&!grants.contains(&key(name)){withheld.insert(name.to_owned());return capture[0].to_owned();}
        values.get(&key(name)).map(|(_,v)|v.clone()).unwrap_or_else(||capture.get(2).map(|m|m.as_str()).unwrap_or("").into())
    }).into_owned()
    };
    let mut config = raw.clone();
    for field in ["command", "url"] {
        if let Some(text) = raw[field].as_str() {
            config[field] = json!(expand(text));
        }
    }
    if let Some(args) = raw["args"].as_array() {
        config["args"] = Value::Array(
            args.iter()
                .map(|v| {
                    v.as_str()
                        .map(|s| json!(expand(s)))
                        .unwrap_or_else(|| v.clone())
                })
                .collect(),
        );
    }
    for field in ["env", "headers"] {
        if let Some(map) = raw[field].as_object() {
            config[field] = Value::Object(
                map.iter()
                    .map(|(name, v)| {
                        (
                            name.clone(),
                            v.as_str()
                                .map(|s| json!(expand(s)))
                                .unwrap_or_else(|| v.clone()),
                        )
                    })
                    .collect(),
            );
        }
    }
    if let Some(id) = raw["oauth"]["clientId"].as_str() {
        config["oauth"]["clientId"] = json!(expand(id));
    }
    if let Some(scopes) = raw["oauth"]["scopes"].as_array() {
        config["oauth"]["scopes"] = Value::Array(
            scopes
                .iter()
                .map(|v| {
                    v.as_str()
                        .map(|s| json!(expand(s)))
                        .unwrap_or_else(|| v.clone())
                })
                .collect(),
        );
    }
    let mut env = Environment::new();
    for (normalized, (name, value)) in values {
        if !package_exclusion(&name)
            && (SAFE_MCP_ENV_NAMES.contains(&normalized.as_str()) || grants.contains(&normalized))
        {
            env.insert(name, value);
        }
    }
    let declared: Environment = config
        .get("env")
        .cloned()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    for (name, value) in explicit.iter().chain(declared.iter()) {
        if windows {
            env.retain(|existing, _| key(existing) != key(name));
        }
        env.insert(name.clone(), value.clone());
    }
    let passed: BTreeSet<_> = env.keys().map(|n| key(n)).collect();
    if raw["type"] == "stdio"
        || (raw.get("type").is_none() && raw.get("url").is_none() && raw["command"].is_string())
    {
        for name in create_clean_mcp_environment(&Environment::new(), ambient).keys() {
            if !passed.contains(&key(name)) {
                withheld.insert(name.clone());
            }
        }
    }
    ResolvedEnvironment {
        config,
        env,
        withheld: withheld.into_iter().collect(),
    }
}
