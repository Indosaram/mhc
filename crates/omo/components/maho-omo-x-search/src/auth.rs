//! Port of omo-senpi `components/x-search/auth.ts` at latest `455dee62`.
//!
//! Synchronous credential gate for load-time extension registration, plus
//! refresh-aware per-call bearer resolution through the live model registry.

use std::collections::BTreeMap;
use std::path::Path;

use maho_ext_api::ModelRegistry;

pub const XAI_PROVIDER: &str = "xai";

/// Upstream `resolveXaiBearer`: resolves bearer from model registry's provider auth if stored,
/// else falls back to trimmed environment variable `XAI_API_KEY`.
pub async fn resolve_xai_bearer(
    model_registry: &(dyn ModelRegistry + '_),
    env: &BTreeMap<String, String>,
) -> Option<(String, &'static str)> {
    if let Ok(Some(_)) = model_registry.get_stored_credential_type(XAI_PROVIDER) {
        match model_registry.get_provider_auth(XAI_PROVIDER).await {
            Ok(Some(resolved)) => {
                let bearer = resolved
                    .auth
                    .api_key
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_owned);
                if let Some(bearer) = bearer {
                    return Some((bearer, "store"));
                }
                return None;
            }
            _ => return None,
        }
    }

    let env_bearer = env
        .get("XAI_API_KEY")
        .map(String::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    env_bearer.map(|bearer| (bearer, "env"))
}

/// Upstream `hasXaiCredential`: synchronous check at extension load time.
/// Checks `<agentDir>/auth.json` for `xai` entry with `type` "oauth" or "api_key".
/// If `auth.json` is missing, falls back to non-empty `XAI_API_KEY` in environment.
/// If `auth.json` exists but is invalid JSON or has no valid xai entry, fails closed (returns false).
pub fn has_xai_credential(agent_dir: &Path, env: &BTreeMap<String, String>) -> bool {
    let auth_path = agent_dir.join("auth.json");
    if !auth_path.exists() {
        return env
            .get("XAI_API_KEY")
            .map(String::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some();
    }

    let Ok(content) = std::fs::read_to_string(&auth_path) else {
        return false;
    };
    let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&content) else {
        return false;
    };
    let Some(obj) = parsed.as_object() else {
        return false;
    };
    let Some(entry) = obj.get(XAI_PROVIDER) else {
        return false;
    };
    let Some(entry_obj) = entry.as_object() else {
        return false;
    };
    let Some(type_val) = entry_obj.get("type").and_then(serde_json::Value::as_str) else {
        return false;
    };
    type_val == "oauth" || type_val == "api_key"
}
