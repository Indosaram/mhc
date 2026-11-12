use crate::contract::{Cancel, CatalogModel, CcResult, LoadedCatalog, ProviderEnv};
use serde_json::{Value, json};
use std::path::Path;

fn object(v: &Value) -> bool {
    v.is_object()
}
fn string_field(v: &Value, key: &str) -> CcResult<String> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("Expected {key} to be a non-empty string"))
}
fn positive(v: &Value, key: &str) -> CcResult<u64> {
    v.get(key)
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| format!("Expected {key} to be a positive number"))
}
fn model(id: String, name: String, context: u64, api_base: &str, decorated: bool) -> CatalogModel {
    let api = crate::catalog::api_for_model_id(&id).to_owned();
    CatalogModel {
        id: id.clone(),
        name: if decorated {
            format!("{name} (CC)")
        } else {
            name
        },
        context_window: context,
        max_tokens: crate::catalog::max_output_tokens_for_model(&id, context),
        reasoning: crate::catalog::is_reasoning_model(&id),
        input: crate::catalog::input_modalities_for_model(&id),
        base_url: crate::contract::base_url_for_model(api_base, &api),
        api,
    }
}
fn require_models(models: Vec<CatalogModel>) -> CcResult<Vec<CatalogModel>> {
    if models.is_empty() {
        Err("Command Code returned an empty model catalog".into())
    } else {
        Ok(models)
    }
}
pub fn command_code_models_from_api_response(
    value: &Value,
    api_base: &str,
) -> CcResult<Vec<CatalogModel>> {
    if !object(value) {
        return Err("Expected models response to be an object".into());
    }
    if value.get("object").and_then(Value::as_str) != Some("list") {
        return Err("Expected models response object to be 'list'".into());
    }
    let data = value
        .get("data")
        .and_then(Value::as_array)
        .ok_or("Expected models response data to be an array")?;
    let mut models = Vec::with_capacity(data.len());
    for entry in data {
        if !object(entry) {
            return Err("Expected model entry to be an object".into());
        }
        models.push(model(
            string_field(entry, "id")?,
            string_field(entry, "name")?,
            positive(entry, "context_length")?,
            api_base,
            true,
        ));
    }
    require_models(models)
}
pub fn command_code_models_from_cache(value: &Value) -> CcResult<Vec<CatalogModel>> {
    if !object(value) {
        return Err("Expected model cache to be an object".into());
    }
    if value.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("Expected model cache version 1".into());
    }
    let entries = value
        .get("models")
        .and_then(Value::as_array)
        .ok_or("Expected cached models to be an array")?;
    let mut result = Vec::new();
    for e in entries {
        if !object(e) {
            return Err("Expected cached model entry to be an object".into());
        }
        let id = string_field(e, "id")?;
        if !e.get("reasoning").is_some_and(Value::is_boolean) {
            return Err("Expected reasoning to be a boolean".into());
        }
        positive(e, "maxTokens")?;
        let context = positive(e, "contextWindow")?;
        let name = string_field(e, "name")?;
        result.push(model(
            id,
            name,
            context,
            crate::contract::DEFAULT_PROVIDER_API_BASE,
            false,
        ));
    }
    require_models(result)
}
pub async fn fetch_models(
    url: &str,
    timeout_ms: u64,
    cancel: Option<Cancel>,
) -> CcResult<Vec<CatalogModel>> {
    if cancel.as_ref().is_some_and(Cancel::is_cancelled) {
        return Err("The operation was aborted".into());
    }
    let work = async {
        let client = reqwest::Client::new();
        let response = client
            .get(url)
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !response.status().is_success() {
            let status = response.status();
            return Err(format!(
                "Failed to fetch Command Code models: {} {}",
                status.as_u16(),
                status.canonical_reason().unwrap_or("")
            ));
        }
        let body: Value = response.json().await.map_err(|e| e.to_string())?;
        command_code_models_from_api_response(&body, crate::contract::DEFAULT_PROVIDER_API_BASE)
    };
    tokio::select! {
        _ = async { if let Some(c) = &cancel { while !c.is_cancelled() { tokio::task::yield_now().await; } } else { std::future::pending::<()>().await } } => Err("The operation was aborted".into()),
        result = tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), work) => match result { Ok(r) => r, Err(_) => Err(format!("Command Code model discovery timed out after {timeout_ms}ms")) }
    }
}
pub fn read_cache(path: &Path, api_base: &str) -> CcResult<Vec<CatalogModel>> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let value: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    let mut models = command_code_models_from_cache(&value)?;
    for m in &mut models {
        m.base_url = crate::contract::base_url_for_model(api_base, &m.api);
    }
    Ok(models)
}
pub fn write_cache(path: &Path, models: &[CatalogModel]) -> CcResult<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let tmp = Path::new(&format!("{}.{}.tmp", path.display(), std::process::id())).to_path_buf();
    let values: Vec<Value> = models.iter().map(|m| json!({"id":m.id,"name":m.name,"api":m.api,"reasoning":m.reasoning,"contextWindow":m.context_window,"maxTokens":m.max_tokens,"input":m.input})).collect();
    let result = (|| -> CcResult<()> {
        let bytes = serde_json::to_vec_pretty(&json!({"version":1,"models":values}))
            .map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&tmp)
                .map_err(|e| e.to_string())?;
            use std::io::Write;
            f.write_all(&bytes).map_err(|e| e.to_string())?;
        }
        #[cfg(not(unix))]
        std::fs::write(&tmp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
pub async fn load_models(env: &ProviderEnv, cancel: Option<Cancel>) -> LoadedCatalog {
    let live = fetch_models(&env.models_url, env.models_timeout_ms, cancel.clone()).await;
    match live {
        Ok(models) => match write_cache(&env.cache_path, &models) {
            Ok(()) => LoadedCatalog {
                models,
                source: crate::contract::CatalogSource::Live,
                warning: None,
            },
            Err(e) => LoadedCatalog {
                models,
                source: crate::contract::CatalogSource::Live,
                warning: Some(format!(
                    "Loaded the live Command Code model catalog but could not update {}: {e}",
                    env.cache_path.display()
                )),
            },
        },
        Err(live_error) => {
            if cancel.as_ref().is_some_and(Cancel::is_cancelled) {
                return LoadedCatalog {
                    models: vec![],
                    source: crate::contract::CatalogSource::Empty,
                    warning: Some(live_error),
                };
            }
            match read_cache(&env.cache_path, &env.api_base) {
                Ok(models) => LoadedCatalog {
                    models,
                    source: crate::contract::CatalogSource::Cache,
                    warning: Some(format!(
                        "Could not refresh the Command Code model catalog ({live_error}). Using the cached catalog from {}.",
                        env.cache_path.display()
                    )),
                },
                Err(cache_error) => LoadedCatalog {
                    models: vec![],
                    source: crate::contract::CatalogSource::Empty,
                    warning: Some(format!(
                        "Could not refresh the Command Code model catalog ({live_error}), and no valid cached catalog is available at {} ({cache_error}). Command Code models will remain unavailable until /commandcode-refresh succeeds.",
                        env.cache_path.display()
                    )),
                },
            }
        }
    }
}
pub fn load_models_sync(env: &ProviderEnv) -> LoadedCatalog {
    let env = env.clone();
    let spawned = std::thread::Builder::new()
        .name("commandcode-catalog".to_owned())
        .spawn(move || {
            match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime.block_on(load_models(&env, None)),
                Err(error) => LoadedCatalog {
                    models: vec![],
                    source: crate::contract::CatalogSource::Empty,
                    warning: Some(format!(
                        "Could not start the Command Code catalog runtime: {error}"
                    )),
                },
            }
        });
    match spawned.map(std::thread::JoinHandle::join) {
        Ok(Ok(loaded)) => loaded,
        Ok(Err(_)) => LoadedCatalog {
            models: vec![],
            source: crate::contract::CatalogSource::Empty,
            warning: Some("The Command Code catalog worker panicked".to_owned()),
        },
        Err(error) => LoadedCatalog {
            models: vec![],
            source: crate::contract::CatalogSource::Empty,
            warning: Some(format!(
                "Could not spawn the Command Code catalog worker: {error}"
            )),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn api_parse() {
        let base = crate::contract::DEFAULT_PROVIDER_API_BASE;
        let v = json!({"object":"list","data":[{"id":"x","name":"x","context_length":1000}]});
        let m = command_code_models_from_api_response(&v, base).unwrap();
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].name, "x (CC)");
        assert_eq!(
            command_code_models_from_api_response(&json!({"object":"not-list","data":[]}), base)
                .unwrap_err(),
            "Expected models response object to be 'list'"
        );
        assert!(
            command_code_models_from_api_response(
                &json!({"object":"list","data":[{"id":"x","name":"x","context_length":0}]}),
                base
            )
            .is_err()
        );
        assert_eq!(
            command_code_models_from_api_response(&json!({"object":"list","data":[]}), base)
                .unwrap_err(),
            "Command Code returned an empty model catalog"
        );
    }
    #[test]
    fn cache_parse_and_round_trip() {
        assert!(command_code_models_from_cache(&json!({"version":2,"models":[]})).is_err());
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("models.json");
        let ms = command_code_models_from_api_response(
            &json!({"object":"list","data":[{"id":"x","name":"x","context_length":1000}]}),
            crate::contract::DEFAULT_PROVIDER_API_BASE,
        )
        .unwrap();
        write_cache(&p, &ms).unwrap();
        assert_eq!(
            read_cache(&p, crate::contract::DEFAULT_PROVIDER_API_BASE)
                .unwrap()
                .iter()
                .map(|m| m.id.clone())
                .collect::<Vec<_>>(),
            vec!["x"]
        );
    }
}
