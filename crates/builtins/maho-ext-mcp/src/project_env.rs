use crate::config::McpConfigValidationError;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
pub const PROJECT_MCP_SAFE_BASE_NAMES: &[&str] = &[
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
pub fn lookup_project_env<'a>(
    env: &'a BTreeMap<String, String>,
    name: &str,
    windows: bool,
) -> Option<&'a String> {
    if !windows {
        return env.get(name);
    }
    env.get(&name.to_uppercase()).or_else(|| {
        env.iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value)
    })
}
pub fn merge_project_env(
    layers: &[&BTreeMap<String, String>],
    windows: bool,
) -> BTreeMap<String, String> {
    let mut result: BTreeMap<String, String> = BTreeMap::new();
    for layer in layers {
        for (key, value) in *layer {
            if windows {
                result.retain(|existing, _| !existing.eq_ignore_ascii_case(key));
            }
            result.insert(key.clone(), value.clone());
        }
    }
    result
}
pub struct ProjectEnvironment {
    pub declaration: Value,
    pub env: BTreeMap<String, String>,
    pub withheld: Vec<String>,
}
pub fn project_mcp_environment(
    raw: &Value,
    trusted: bool,
    granted: &[String],
    explicit: Option<&BTreeMap<String, String>>,
    ambient: &BTreeMap<String, String>,
) -> Result<ProjectEnvironment, McpConfigValidationError> {
    let windows = cfg!(windows);
    let empty = BTreeMap::new();
    let explicit = explicit.unwrap_or(&empty);
    let remote = raw["type"].as_str().unwrap_or(if raw.get("url").is_some() {
        "http"
    } else {
        "stdio"
    }) != "stdio";
    let mut inherited = BTreeMap::new();
    for name in PROJECT_MCP_SAFE_BASE_NAMES
        .iter()
        .copied()
        .chain(granted.iter().map(String::as_str))
    {
        let lower = name.to_ascii_lowercase();
        if ["npm_", "npm-", "yarn_", "yarn-", "pnpm_", "pnpm-"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
        {
            continue;
        }
        if let Some(value) = lookup_project_env(ambient, name, windows) {
            inherited.insert(name.to_owned(), value.clone());
        }
    }
    let mut withheld = BTreeSet::new();
    let references = regex::Regex::new(r"\$\{([A-Za-z_][A-Za-z0-9_]*)(:-([^}]*))?\}")
        .expect("static env pattern");
    let granted_name = |name: &str| {
        granted.iter().any(|entry| {
            if windows {
                entry.eq_ignore_ascii_case(name)
            } else {
                entry == name
            }
        })
    };
    let mut expand =
        |text: &str, field: &str| -> Result<String, McpConfigValidationError> {
            if !remote && trusted && (text.trim_start().starts_with('!') || text.contains("$(")) {
                crate::config::interpolate_value(&Value::String(text.into()), field, &empty)?;
            }
            let mut result = String::new();
            let mut end = 0;
            for captures in references.captures_iter(text) {
                let whole = captures.get(0).expect("whole match");
                let name = &captures[1];
                result.push_str(&text[end..whole.start()]);
                if (!remote && trusted) || granted_name(name) {
                    let value = if granted_name(name) {
                        lookup_project_env(explicit, name, windows)
                            .or_else(|| lookup_project_env(ambient, name, windows))
                    } else if explicit.is_empty() {
                        lookup_project_env(ambient, name, windows)
                    } else {
                        lookup_project_env(explicit, name, windows)
                    };
                    let env = value
                        .map(|value| BTreeMap::from([(name.into(), value.clone())]))
                        .unwrap_or_default();
                    let expanded = crate::config::interpolate_value(
                        &Value::String(whole.as_str().into()),
                        field,
                        &env,
                    )?;
                    result.push_str(expanded.as_str().ok_or_else(|| {
                        McpConfigValidationError("Invalid MCP interpolation".into())
                    })?);
                } else {
                    withheld.insert(name.to_owned());
                    result.push_str(whole.as_str());
                }
                end = whole.end();
            }
            result.push_str(&text[end..]);
            Ok(result)
        };
    let mut declaration = raw.clone();
    for field in ["command", "cwd", "url"] {
        if let Some(text) = raw[field].as_str() {
            declaration[field] = expand(text, field)?.into();
        }
    }
    for field in ["args"] {
        if let Some(values) = raw[field].as_array() {
            declaration[field] = Value::Array(
                values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .map(|text| expand(text, field).map(Value::String))
                            .unwrap_or_else(|| Ok(value.clone()))
                    })
                    .collect::<Result<_, _>>()?,
            );
        }
    }
    for field in ["env", "headers"] {
        if let Some(values) = raw[field].as_object() {
            for (key, value) in values {
                if let Some(text) = value.as_str() {
                    declaration[field][key] = expand(text, &format!("{field}.{key}"))?.into();
                }
            }
        }
    }
    if let Some(oauth) = raw["oauth"].as_object() {
        for field in ["clientId", "clientMetadataUrl"] {
            if let Some(text) = oauth.get(field).and_then(Value::as_str) {
                declaration["oauth"][field] = expand(text, field)?.into();
            }
        }
        if let Some(scopes) = oauth.get("scopes").and_then(Value::as_array) {
            declaration["oauth"]["scopes"] = Value::Array(
                scopes
                    .iter()
                    .map(|scope| {
                        scope
                            .as_str()
                            .map(|text| expand(text, "oauth.scopes").map(Value::String))
                            .unwrap_or_else(|| Ok(scope.clone()))
                    })
                    .collect::<Result<_, _>>()?,
            );
        }
    }
    if remote
        && (raw["auth"] == "bearer"
            || (raw.get("auth").is_none() && raw.get("bearerTokenEnv").is_some()))
        && let Some(name) = raw["bearerTokenEnv"].as_str()
        && !granted_name(name)
    {
        withheld.insert(name.into());
        declaration
            .as_object_mut()
            .expect("declaration")
            .remove("bearerTokenEnv");
        declaration["auth"] = false.into();
    }
    let declared = declaration["env"]
        .as_object()
        .map(|map| {
            map.iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into())))
                .collect()
        })
        .unwrap_or_default();
    Ok(ProjectEnvironment {
        declaration,
        env: merge_project_env(&[&inherited, explicit, &declared], windows),
        withheld: withheld.into_iter().collect(),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_never_inherits_ungranted_bearer_even_when_trusted() {
        let raw =
            serde_json::json!({"url":"https://example.test/${TOKEN}","bearerTokenEnv":"TOKEN"});
        let ambient = BTreeMap::from([
            ("TOKEN".into(), "secret".into()),
            ("PATH".into(), "/bin".into()),
        ]);
        let result = project_mcp_environment(&raw, true, &[], None, &ambient).unwrap();
        assert_eq!(result.withheld, ["TOKEN"]);
        assert_eq!(result.declaration["auth"], false);
        assert!(!result.env.contains_key("TOKEN"));
        assert_eq!(result.env["PATH"], "/bin");
    }
    #[test]
    fn windows_layers_replace_case_variants() {
        let a = BTreeMap::from([("Path".into(), "a".into())]);
        let b = BTreeMap::from([("PATH".into(), "b".into())]);
        assert_eq!(merge_project_env(&[&a, &b], true), b);
    }
}
