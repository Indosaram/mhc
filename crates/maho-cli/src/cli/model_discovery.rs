//! Port of senpi `core/model-discovery.ts`; discovery preserves the user's unlisted models.
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const LEVELS: [&str; 7] = ["off", "minimal", "low", "medium", "high", "xhigh", "max"];
type Efforts = (Vec<String>, Option<String>, Vec<String>);
type ModelEfforts = (String, Vec<String>, Option<String>, Vec<String>);

#[derive(Default)]
pub struct DiscoveryReport {
    pub provider: String,
    pub url: String,
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub unchanged: Vec<String>,
    pub unlisted: Vec<String>,
    pub efforts: Vec<ModelEfforts>,
    pub efforts_ignored: bool,
    pub backup: Option<PathBuf>,
    pub comments_dropped: bool,
}

fn with_efforts(model: &mut Value, advertised: &Value) -> Option<Efforts> {
    let entries = advertised.as_array()?;
    let mut map = serde_json::Map::new();
    let mut unmapped = Vec::new();
    let mut default = None;
    for entry in entries {
        let Some(value) = entry
            .as_str()
            .or_else(|| entry.get("value").and_then(Value::as_str))
            .filter(|s| !s.trim().is_empty())
        else {
            continue;
        };
        let normalized = value.trim().to_lowercase();
        let level = if normalized == "none" {
            "off"
        } else {
            &normalized
        };
        if !LEVELS.contains(&level) {
            if !unmapped.iter().any(|existing| existing == value) {
                unmapped.push(value.to_owned());
            }
            continue;
        }
        map.entry(level.to_owned()).or_insert_with(|| json!(value));
        if default.is_none() && entry.get("default") == Some(&Value::Bool(true)) {
            default = Some(level.to_owned());
        }
    }
    let levels: Vec<_> = LEVELS
        .iter()
        .filter(|level| map.contains_key(**level))
        .map(|level| (*level).to_owned())
        .collect();
    let object = model.as_object_mut()?;
    object.remove("thinkingLevelMap");
    object.remove("defaultThinkingLevel");
    object.insert("reasoning".into(), json!(!levels.is_empty()));
    if !levels.is_empty() {
        for level in LEVELS {
            map.entry(level).or_insert(Value::Null);
        }
        object.insert("thinkingLevelMap".into(), Value::Object(map));
        if let Some(default) = &default {
            object.insert("defaultThinkingLevel".into(), json!(default));
        }
    }
    Some((levels, default, unmapped))
}

fn merge(provider: &mut Value, payload: Value, report: &mut DiscoveryReport) -> Result<(), String> {
    let listing = payload
        .as_array()
        .or_else(|| payload.get("data").and_then(Value::as_array))
        .ok_or("Endpoint did not return a model list")?;
    let honor = provider.pointer("/compat/supportsReasoningEffort") == Some(&Value::Bool(true));
    let mut configured = provider
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut seen = Vec::new();
    for entry in listing {
        let Some(id) = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
        else {
            continue;
        };
        if seen.iter().any(|previous| previous == id) {
            continue;
        }
        seen.push(id.to_owned());
        let index = configured
            .iter()
            .position(|model| model.get("id").and_then(Value::as_str) == Some(id));
        let mut next = index.map_or_else(|| json!({"id":id}), |index| configured[index].clone());
        let efforts = entry.get("reasoning_efforts").unwrap_or(&Value::Null);
        if honor {
            if let Some((levels, default, unmapped)) = with_efforts(&mut next, efforts) {
                report.efforts.push((id.into(), levels, default, unmapped));
            }
        } else if efforts.is_array() {
            report.efforts_ignored = true;
        }
        match index {
            None => {
                configured.push(next);
                report.added.push(id.into());
            }
            Some(index) if configured[index] == next => report.unchanged.push(id.into()),
            Some(index) => {
                configured[index] = next;
                report.updated.push(id.into());
            }
        }
    }
    report.unlisted = configured
        .iter()
        .filter_map(|model| model.get("id").and_then(Value::as_str))
        .filter(|id| !seen.iter().any(|seen| seen == id))
        .map(str::to_owned)
        .collect();
    provider["models"] = Value::Array(configured);
    Ok(())
}

fn write_with_backup(path: &Path, original: &str, next: &str) -> Result<PathBuf, String> {
    let permissions = fs::metadata(path).map_err(|e| e.to_string())?.permissions();
    let mut suffix = 0u64;
    let (backup, mut file) = loop {
        let backup = PathBuf::from(format!(
            "{}.bak{}",
            path.display(),
            if suffix == 0 {
                String::new()
            } else {
                format!(".{suffix}")
            }
        ));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
            options.mode(permissions.mode() & 0o777);
        }
        match options.open(&backup) {
            Ok(file) => break (backup, file),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => suffix += 1,
            Err(e) => return Err(e.to_string()),
        }
    };
    let result = (|| -> Result<(), String> {
        file.write_all(original.as_bytes())
            .map_err(|e| e.to_string())?;
        let mut temporary =
            tempfile::NamedTempFile::new_in(path.parent().unwrap_or(Path::new(".")))
                .map_err(|e| e.to_string())?;
        temporary
            .as_file()
            .set_permissions(permissions)
            .map_err(|e| e.to_string())?;
        temporary
            .write_all(next.as_bytes())
            .map_err(|e| e.to_string())?;
        if fs::read_to_string(path).map_err(|e| e.to_string())? != original {
            return Err(format!(
                "{} changed during discovery; nothing was written",
                path.display()
            ));
        }
        temporary.persist(path).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if let Err(error) = result {
        if let Err(cleanup) = fs::remove_file(&backup) {
            eprintln!(
                "Cannot remove discovery backup {}: {cleanup}",
                backup.display()
            );
        }
        return Err(error);
    }
    Ok(backup)
}

pub async fn discover_provider_models(
    path: &Path,
    provider_id: &str,
) -> Result<DiscoveryReport, String> {
    let original =
        fs::read_to_string(path).map_err(|e| format!("Cannot read {}: {e}", path.display()))?;
    let mut document = Value::Object(maho_core::settings_manager::parse_settings_json(&original)?);
    let wanted = maho_ai::legacy_provider_ids::normalize_provider_id(provider_id);
    let key = document
        .get("providers")
        .and_then(Value::as_object)
        .and_then(|providers| {
            providers
                .keys()
                .find(|key| maho_ai::legacy_provider_ids::normalize_provider_id(key) == wanted)
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "Provider \"{provider_id}\" is not defined in {}",
                path.display()
            )
        })?;
    let provider = &document["providers"][&key];
    if !provider.is_object() {
        return Err(format!("Provider \"{provider_id}\" is not an object"));
    }
    if let Some(api) = provider.get("api").and_then(Value::as_str)
        && !matches!(api, "openai-completions" | "openai-responses")
    {
        return Err(format!(
            "Provider \"{provider_id}\" uses api \"{api}\", not an OpenAI-compatible api"
        ));
    }
    let base = provider
        .get("baseUrl")
        .and_then(Value::as_str)
        .ok_or("Provider needs a baseUrl to discover models")?;
    let (url, display) = super::model_discovery_url::listing_url(base)?;
    let configured: Option<HashMap<String, String>> = provider
        .get("headers")
        .map(|headers| serde_json::from_value(headers.clone()))
        .transpose()
        .map_err(|e| e.to_string())?;
    let configured = maho_core::resolve_config_value::resolve_headers_or_throw(
        configured.as_ref(),
        &format!("provider \"{provider_id}\""),
        None,
    )
    .await?
    .unwrap_or_default();
    let agent = path.parent().unwrap_or(Path::new("."));
    let runtime = maho_core::model_runtime::ModelRuntime::create(
        maho_core::model_runtime::CreateModelRuntimeOptions {
            models_path: Some(path.into()),
            auth_path: Some(agent.join("auth.json")),
            agent_dir: Some(agent.into()),
            allow_model_network: Some(false),
            ..Default::default()
        },
    )
    .await.map_err(|error| error.to_string())?;
    let auth = if runtime.get_provider(&wanted).is_some() {
        runtime.get_auth(&wanted).await.map_err(|e| e.to_string())?
    } else {
        None
    };
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static("application/json"),
    );
    for (name, value) in configured {
        headers.insert(
            reqwest::header::HeaderName::from_bytes(name.as_bytes()).map_err(|e| e.to_string())?,
            value
                .parse()
                .map_err(|e: reqwest::header::InvalidHeaderValue| e.to_string())?,
        );
    }
    if let Some(auth) = auth {
        for (name, value) in auth.auth.headers.unwrap_or_default() {
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|e| e.to_string())?;
            if let Some(value) = value {
                headers.insert(
                    name,
                    value
                        .parse()
                        .map_err(|e: reqwest::header::InvalidHeaderValue| e.to_string())?,
                );
            } else {
                headers.remove(name);
            }
        }
        if let Some(key) = auth.auth.api_key.filter(|key| !key.is_empty())
            && !headers.contains_key(reqwest::header::AUTHORIZATION)
        {
            headers.insert(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {key}")
                    .parse()
                    .map_err(|e: reqwest::header::InvalidHeaderValue| e.to_string())?,
            );
        }
    }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())?;
    let response = client
        .get(url)
        .headers(headers)
        .send()
        .await
        .map_err(|error| format!("GET {display} failed: {}", error.without_url()))?;
    if !response.status().is_success() {
        return Err(format!("GET {display} failed: HTTP {}", response.status()));
    }
    let payload = response
        .json::<Value>()
        .await
        .map_err(|_| format!("GET {display} did not return JSON"))?;
    let mut report = DiscoveryReport {
        provider: key.clone(),
        url: display,
        ..Default::default()
    };
    merge(&mut document["providers"][&key], payload, &mut report)?;
    if report.added.is_empty() && report.updated.is_empty() {
        return Ok(report);
    }
    let next = format!(
        "{}\n",
        serde_json::to_string_pretty(&document).map_err(|e| e.to_string())?
    );
    if let Some(error) =
        maho_core::model_config::ModelConfig::parse(&next, &path.to_string_lossy()).get_error()
    {
        return Err(format!(
            "Discovery would leave an invalid models.json, so nothing was written.\n{error}"
        ));
    }
    report.backup = Some(write_with_backup(path, &original, &next)?);
    report.comments_dropped = contains_comments(&original);
    Ok(report)
}

fn contains_comments(text: &str) -> bool {
    let mut quoted = false;
    let mut escaped = false;
    let mut bytes = text.bytes().peekable();
    while let Some(byte) = bytes.next() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        } else if byte == b'/' && matches!(bytes.peek(), Some(b'/' | b'*')) {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merge_preserves_unlisted_models_and_maps_advertised_efforts() {
        // Given configured custom fields and an unlisted model.
        let mut provider = json!({"compat":{"supportsReasoningEffort":true},"models":[{"id":"a","name":"Custom","thinkingLevelMap":{"high":"stale"}},{"id":"kept"}]});
        let mut report = DiscoveryReport::default();
        // When the endpoint advertises one changed and one new model, including a duplicate.
        merge(&mut provider, json!({"data":[{"id":"a","reasoning_efforts":["none",{"value":"High","default":true},"turbo"]},{"id":"new"},{"id":"new"}]}), &mut report).expect("merge");
        // Then discovery owns only reasoning fields and preserves unlisted configuration.
        assert_eq!(provider["models"][0]["name"], "Custom");
        assert_eq!(provider["models"][0]["thinkingLevelMap"]["high"], "High");
        assert_eq!(
            provider["models"][0]["thinkingLevelMap"]["low"],
            Value::Null
        );
        assert_eq!(provider["models"][0]["defaultThinkingLevel"], "high");
        assert_eq!(report.added, ["new"]);
        assert_eq!(report.updated, ["a"]);
        assert_eq!(report.unlisted, ["kept"]);
    }
    #[test]
    fn empty_advertised_ladder_clears_stale_reasoning_controls() {
        // Given a model with stale reasoning controls.
        let mut model =
            json!({"id":"a","defaultThinkingLevel":"high","thinkingLevelMap":{"high":"high"}});
        // When the endpoint advertises no representable levels.
        with_efforts(&mut model, &json!(["turbo"]));
        // Then reasoning is disabled and stale controls are removed.
        assert_eq!(model, json!({"id":"a","reasoning":false}));
    }
    #[test]
    fn backup_keeps_original_and_rejects_concurrent_edits() {
        // Given an original document and a newer on-disk edit.
        let dir = tempfile::tempdir().expect("directory");
        let path = dir.path().join("models.json");
        fs::write(&path, "newer").expect("fixture");
        // When discovery attempts to replace its stale snapshot.
        let result = write_with_backup(&path, "original", "discovered");
        // Then the concurrent edit survives and no backup is left behind.
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&path).expect("document"), "newer");
        assert_eq!(fs::read_dir(dir.path()).expect("entries").count(), 1);
    }
}
