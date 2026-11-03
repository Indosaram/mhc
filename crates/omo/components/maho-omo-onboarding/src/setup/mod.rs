//! `omo setup`'s detection and writers, ported from omo v5.1.52 `packages/omo-native/bin/lib`
//! (`setup-stream.js` and the modules it imports). The engine's onboarding scan
//! ([`crate::scan_runtime`]) reaches them through [`stream::NativeSetupBackend`], the
//! [`crate::scan_runtime::SetupBackend`] that reads this machine; `omo setup` and the scan share one
//! import logic, as upstream.
//!
//! File map (one upstream file per module): `agent-dir.js` -> [`agent_dir`], `auth-store.js` ->
//! [`auth_store`], `jsonc.js` -> [`jsonc`], `jsonc-edit.js` -> [`jsonc_edit`], `sqlite-rows.js` ->
//! [`sqlite_rows`], `setup-detect.js` -> [`detect`], `setup-guidance.js` -> [`guidance`],
//! `setup-credentials.js` -> [`credentials`], `setup-opencode-assets.js` -> [`opencode_assets`],
//! `setup-assets-import.js` -> [`assets_import`], `setup-opencode-providers.js` ->
//! [`opencode_providers`], `setup-providers-import.js` -> [`providers_import`],
//! `setup-opencode-models.js` -> [`opencode_models`], `engine-models.js` -> [`engine_models`],
//! `setup-model-choices-import.js` -> [`model_choices_import`], `setup-stream-items.js` ->
//! [`stream_items`], `setup-stream.js` -> [`stream`]. `provider-map.json` is embedded verbatim.

pub mod agent_dir;
pub mod assets_import;
pub mod auth_store;
pub mod credentials;
pub mod detect;
pub mod engine_models;
pub mod guidance;
pub mod jsonc;
pub mod jsonc_edit;
pub mod model_choices_import;
pub mod opencode_assets;
pub mod opencode_models;
pub mod opencode_providers;
pub mod provider_map;
pub mod providers_import;
pub mod sqlite_rows;
pub mod stream;
pub mod stream_items;

use std::collections::BTreeMap;

/// Where a setup source reads: upstream's `runtime` (`{home, env}`), plus the seams the tests and
/// the engine inject (the offline model catalog, the plugin root holding bundled skills).
#[derive(Clone)]
pub struct SetupRuntime {
    pub home: String,
    pub env: BTreeMap<String, String>,
    /// Upstream `runtime.loadModelCatalog`: the engine's builtin provider catalog.
    pub load_model_catalog: Option<engine_models::LoadCatalog>,
    /// Upstream `runtime.pluginRoot`: the directory whose `skills/` holds the bundled skills.
    pub plugin_root: Option<std::path::PathBuf>,
}

impl SetupRuntime {
    pub fn new(home: impl Into<String>, env: BTreeMap<String, String>) -> Self {
        Self {
            home: home.into(),
            env,
            load_model_catalog: None,
            plugin_root: None,
        }
    }

    /// `env[name]`, treating an empty value as unset (JS `env.X || fallback`).
    pub(crate) fn env_value(&self, name: &str) -> Option<&str> {
        self.env
            .get(name)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }
}

/// `new Date().toISOString().replace(/[-:]/g, "")`: the backup suffix every writer uses.
pub fn timestamp() -> String {
    let now = jiff::Timestamp::now();
    now.strftime("%Y%m%dT%H%M%S.%3fZ").to_string()
}

pub(crate) fn sorted_unique(values: impl IntoIterator<Item = String>) -> Vec<String> {
    let set: std::collections::BTreeSet<String> = values.into_iter().collect();
    set.into_iter().collect()
}

/// `${label}: ${ids.join(", ") || "none"}`.
pub(crate) fn list(label: &str, ids: &[String]) -> String {
    format!(
        "{label}: {}",
        if ids.is_empty() {
            "none".to_owned()
        } else {
            ids.join(", ")
        }
    )
}

/// Writes `bytes` to `path` through a sibling temporary file renamed over it (mode 0600 on Unix).
pub(crate) fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let temporary =
        std::path::PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()));
    let result = (|| {
        std::fs::write(&temporary, bytes)?;
        set_private(&temporary)?;
        std::fs::rename(&temporary, path)
    })();
    if temporary.exists() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
pub(crate) fn set_private(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
pub(crate) fn set_private(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

/// `mkdirSync(dir, { recursive: true, mode: 0o700 })`.
pub(crate) fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

/// JavaScript `String(value)` for the values the setup notices print.
pub(crate) fn js_string(value: Option<&serde_json::Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Null) => "null".into(),
        Some(other) => other.to_string(),
    }
}
