//! Port of senpi packages/coding-agent/src/cli/help-extension-flags.ts.
//!
//! senpi builds a flags-only `DefaultResourceLoader` with the real settings, the CLI extension paths
//! and the extension factories. Natively that resource load is the assembled builtin factories
//! (OAuth lanes and the omo mount included, filtered by the builtin-extension settings) plus the
//! discovered settings/`-e` extensions, loaded for their flags only.
use std::path::{Path, PathBuf};

use maho_ext_api::{ExtensionFlag, FlagType};
use serde_json::{json, Value};

pub struct HelpExtensionFlagsResult {
    pub flags: Vec<Value>,
    pub extension_paths: Vec<PathBuf>,
}

pub fn help_flag_value(flag: &ExtensionFlag) -> Value {
    json!({
        "name": flag.name,
        "type": match flag.kind { FlagType::Boolean { .. } => "boolean", FlagType::String { .. } => "string" },
        "description": flag.description,
        "extensionPath": flag.extension_path,
    })
}

/// Load extensions for their CLI flags and nothing else.
pub async fn resolve_help_extension_flags(
    cwd: &Path,
    agent_dir: &str,
    mut settings_manager: maho_core::settings_manager::SettingsManager,
    additional_extension_paths: &[String],
    no_extensions: bool,
) -> Result<HelpExtensionFlagsResult, String> {
    let (widget_sender, _widget_requests) = tokio::sync::mpsc::unbounded_channel();
    let factories = super::default_extensions::assembled_factories(
        widget_sender, std::sync::Arc::new(std::sync::OnceLock::new()), &settings_manager, cwd, agent_dir,
    );
    let cwd_text = cwd.to_string_lossy();
    // senpi help renders flags only; extensions that cannot load contribute none and are not reported.
    let (factories, _errors) = super::default_extensions::with_user_extensions(
        factories, &mut settings_manager, &cwd_text, agent_dir, additional_extension_paths, no_extensions,
    ).await?;
    let loaded = maho_ext_host::loader::load_extensions_async(factories, cwd, Default::default()).await;
    let mut result = HelpExtensionFlagsResult { flags: Vec::new(), extension_paths: Vec::new() };
    for extension in &loaded.extensions {
        result.flags.extend(extension.flags.iter().map(help_flag_value));
        result.extension_paths.push(PathBuf::from(&extension.identity.resolved_path));
    }
    Ok(result)
}
