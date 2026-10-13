//! Port of senpi packages/coding-agent/src/cli/help-extension-flags.ts.
//!
//! deviation: senpi loads every resolved extension through a flags-only resource loader. The native
//! port loads the statically registered native extensions (as `--list-models` does). Settings-listed
//! user extensions that fail native discovery cannot register flags anyway.
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

pub fn resolve_help_extension_flags(cwd: &Path, no_extensions: bool) -> HelpExtensionFlagsResult {
    let loaded = super::extension_registry::load_native_extensions(cwd, Default::default());
    let extensions = loaded.extensions.iter().filter(|extension| !no_extensions || extension.source_info.source != "user");
    let mut result = HelpExtensionFlagsResult { flags: Vec::new(), extension_paths: Vec::new() };
    for extension in extensions {
        result.flags.extend(extension.flags.iter().map(help_flag_value));
        result.extension_paths.push(PathBuf::from(&extension.identity.resolved_path));
    }
    result
}
