//! Port of senpi `core/resource-loader.ts` global-default extension shims
//! (`ensureGlobalDefaultExtensions`, `getGlobalDefaultExtensionShimPath`,
//! `buildGlobalDefaultExtensionShim`, `resolveGeneratedGlobalDefaultExtensionFactory`).
//!
//! Each default (`diff`, `files`, `prompt-url-widget`, `tps`) is loaded through a generated
//! `<agentDir>/extensions/<id>.js` shim. A missing shim is written; a file a person authored in its
//! place (no generated banner) replaces the default, so the native default is not registered. The
//! native defaults are linked into the binary, so the shim names a `builtin:<id>` specifier instead of
//! an on-disk module; an existing generated shim from another build is kept rather than rewritten so a
//! shared agent directory does not flip between builds.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use maho_core::generated_shim_banner::{generated_shim_banner, has_generated_shim_banner};

use super::extension_registry::GLOBAL_DEFAULT_EXTENSION_IDS;

/// `getGlobalDefaultExtensionShimPath`.
pub fn global_default_extension_shim_path(agent_dir: &Path, extension_id: &str) -> PathBuf {
    agent_dir.join("extensions").join(format!("{extension_id}.js"))
}

/// `buildGlobalDefaultExtensionShim` for a natively linked default.
pub fn build_global_default_extension_shim(extension_id: &str) -> String {
    format!("{}export {{ default }} from \"builtin:{extension_id}\";\n", generated_shim_banner())
}

/// `ensureGlobalDefaultExtensions`: write every absent shim.
pub fn ensure_global_default_extensions(agent_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(agent_dir.join("extensions"))?;
    for extension_id in GLOBAL_DEFAULT_EXTENSION_IDS {
        let target = global_default_extension_shim_path(agent_dir, extension_id);
        if !target.exists() {
            std::fs::write(&target, build_global_default_extension_shim(extension_id))?;
        }
    }
    Ok(())
}

/// Default ids whose shim is present and generated; only these load the native default.
pub fn active_global_default_extension_ids(agent_dir: &Path) -> BTreeSet<&'static str> {
    GLOBAL_DEFAULT_EXTENSION_IDS.iter().copied().filter(|extension_id| {
        std::fs::read_to_string(global_default_extension_shim_path(agent_dir, extension_id))
            .is_ok_and(|content| has_generated_shim_banner(&content))
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_shims_are_written_and_activate_every_default() {
        let agent = tempfile::tempdir().expect("agent dir");
        ensure_global_default_extensions(agent.path()).expect("write shims");
        let diff = std::fs::read_to_string(global_default_extension_shim_path(agent.path(), "diff")).expect("diff shim");
        assert_eq!(diff, build_global_default_extension_shim("diff"));
        assert_eq!(active_global_default_extension_ids(agent.path()).len(), GLOBAL_DEFAULT_EXTENSION_IDS.len());
    }

    #[test]
    fn user_authored_file_replaces_the_default_and_is_kept() {
        let agent = tempfile::tempdir().expect("agent dir");
        std::fs::create_dir_all(agent.path().join("extensions")).expect("extensions dir");
        let tps = global_default_extension_shim_path(agent.path(), "tps");
        std::fs::write(&tps, "export default function () {}\n").expect("user file");
        ensure_global_default_extensions(agent.path()).expect("write shims");
        assert_eq!(std::fs::read_to_string(&tps).expect("user file kept"), "export default function () {}\n");
        assert!(!active_global_default_extension_ids(agent.path()).contains("tps"));
    }

    #[test]
    fn foreign_generated_shim_is_kept_and_active() {
        let agent = tempfile::tempdir().expect("agent dir");
        std::fs::create_dir_all(agent.path().join("extensions")).expect("extensions dir");
        let files = global_default_extension_shim_path(agent.path(), "files");
        let foreign = format!("{}export {{ default }} from \"file:///other/files.js\";\n", maho_core::generated_shim_banner::HISTORICAL_SHIM_BANNERS[0]);
        std::fs::write(&files, &foreign).expect("foreign shim");
        ensure_global_default_extensions(agent.path()).expect("write shims");
        assert_eq!(std::fs::read_to_string(&files).expect("foreign shim kept"), foreign);
        assert!(active_global_default_extension_ids(agent.path()).contains("files"));
    }
}
