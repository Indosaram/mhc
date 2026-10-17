use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[error("codemode runtime asset {package_relative_path} is unavailable: it is not embedded in this binary or could not be extracted, and {local_path} is not readable")]
pub struct CodemodeRuntimeAssetMissingError {
    pub package_relative_path: PathBuf,
    pub local_path: PathBuf,
}

/// Where `package_relative_path` lives in this crate's source tree (`kernels/*` under `assets/`).
pub fn build_tree_runtime_asset_path(package_relative_path: &Path) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    if package_relative_path.starts_with("kernels") { root.join("assets").join(package_relative_path) } else { root.join(package_relative_path) }
}

/// An existing explicit `local_override` wins. Otherwise the copy embedded in this binary is used, so the
/// scripts always match the Rust host they talk to; the build tree is only a last resort, because a
/// checkout at the compile-time path may hold a different revision or be gone.
pub fn require_codemode_runtime_asset(local_override: Option<&Path>, package_relative_path: &Path) -> Result<PathBuf, CodemodeRuntimeAssetMissingError> {
    if let Some(path) = local_override.filter(|path| path.exists()) { return Ok(path.into()); }
    if let Some(embedded) = materialize_embedded_runtime_asset(package_relative_path) { return Ok(embedded); }
    let build_tree = build_tree_runtime_asset_path(package_relative_path);
    if build_tree.exists() { return Ok(build_tree); }
    Err(CodemodeRuntimeAssetMissingError {
        package_relative_path: package_relative_path.into(),
        local_path: local_override.map_or(build_tree, Path::to_path_buf),
    })
}

/// Kernel scripts and the bun-1-4 skill tree compiled into the binary, keyed by package-relative path. A distributed
/// binary runs away from the build tree, so these are extracted on demand. Their upstream senpi provenance is
/// recorded in `assets/UPSTREAM.json`; `scripts/codemode-upstream-diff.py` reports upstream drift.
const EMBEDDED_RUNTIME_ASSETS: &[(&str, &str)] = &[
    ("kernels/js/display-image.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/display-image.js"))),
    ("kernels/js/handles.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/handles.js"))),
    ("kernels/js/inline-worker-entry.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/inline-worker-entry.js"))),
    ("kernels/js/kernel-tools-context.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/kernel-tools-context.js"))),
    ("kernels/js/kernel-tools-define.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/kernel-tools-define.js"))),
    ("kernels/js/kernel-tools-errors.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/kernel-tools-errors.js"))),
    ("kernels/js/kernel-tools-naming.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/kernel-tools-naming.js"))),
    ("kernels/js/kernel-tools-parse.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/kernel-tools-parse.js"))),
    ("kernels/js/kernel-tools-pump.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/kernel-tools-pump.js"))),
    ("kernels/js/kernel-tools-registry.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/kernel-tools-registry.js"))),
    ("kernels/js/kernel-tools-schema.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/kernel-tools-schema.js"))),
    ("kernels/js/kernel-tools-scope.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/kernel-tools-scope.js"))),
    ("kernels/js/process-tree.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/process-tree.js"))),
    ("kernels/js/windows-tree-kill.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/windows-tree-kill.js"))),
    ("kernels/js/worker-core.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-core.js"))),
    ("kernels/js/worker-entry.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-entry.js"))),
    ("kernels/js/worker-indirect-eval.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-indirect-eval.js"))),
    ("kernels/js/worker-heap.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-heap.js"))),
    ("kernels/js/worker-global-sizes.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-global-sizes.js"))),
    ("kernels/js/worker-memory.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-memory.js"))),
    ("kernels/js/worker-runtime.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-runtime.js"))),
    ("kernels/js/worker-shadow-guard.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-shadow-guard.js"))),
    ("kernels/js/worker-shell-capture.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-shell-capture.js"))),
    ("kernels/js/workpool.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/workpool.js"))),
    ("skill/bun-1-4/SKILL.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/SKILL.md"))),
    ("skill/bun-1-4/references/breaking-changes.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/breaking-changes.md"))),
    ("skill/bun-1-4/references/build.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/build.md"))),
    ("skill/bun-1-4/references/dev-tooling.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/dev-tooling.md"))),
    ("skill/bun-1-4/references/http-networking.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/http-networking.md"))),
    ("skill/bun-1-4/references/install.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/install.md"))),
    ("skill/bun-1-4/references/node-compat-platforms.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/node-compat-platforms.md"))),
    ("skill/bun-1-4/references/performance.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/performance.md"))),
    ("skill/bun-1-4/references/runtime-apis.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/runtime-apis.md"))),
    ("skill/bun-1-4/references/security.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/security.md"))),
    ("skill/bun-1-4/references/test.md", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/skill/bun-1-4/references/test.md"))),
];

/// Writes every embedded asset once into a content-addressed directory (scripts import their
/// siblings) and returns the extracted path for `package_relative_path`, or `None` when it is not embedded
/// or extraction failed.
pub fn materialize_embedded_runtime_asset(package_relative_path: &Path) -> Option<PathBuf> {
    static ROOT: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    let relative = package_relative_path.to_str()?;
    EMBEDDED_RUNTIME_ASSETS.iter().find(|(path, _)| *path == relative)?;
    let root = ROOT.get_or_init(|| match extract_embedded_runtime_assets(&std::env::temp_dir()) {
        Ok(root) => Some(root),
        Err(error) => { maho_ext_api::diagnostic(&format!("codemode runtime assets could not be extracted: {error}")); None }
    }).as_ref()?;
    Some(root.join(relative))
}

pub fn extract_embedded_runtime_assets(base: &Path) -> std::io::Result<PathBuf> {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    for (path, content) in EMBEDDED_RUNTIME_ASSETS { hasher.update(path.as_bytes()); hasher.update([0]); hasher.update(content.as_bytes()); hasher.update([0]); }
    let digest = hasher.finalize().iter().take(8).map(|byte| format!("{byte:02x}")).collect::<String>();
    let root = base.join(format!("maho-codemode-assets-{digest}"));
    for (path, content) in EMBEDDED_RUNTIME_ASSETS {
        let target = root.join(path);
        if std::fs::read(&target).is_ok_and(|existing| existing == content.as_bytes()) { continue; }
        let parent = target.parent().unwrap_or(&root);
        std::fs::create_dir_all(parent)?;
        let staging = parent.join(format!(".{}.{}.tmp", target.file_name().and_then(|name| name.to_str()).unwrap_or("asset"), uuid::Uuid::new_v4()));
        std::fs::write(&staging, content)?;
        std::fs::rename(&staging, &target)?;
    }
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_assets_extract_with_their_siblings() {
        let base = tempfile::tempdir().expect("temp dir");
        let root = extract_embedded_runtime_assets(base.path()).expect("extract");
        for (path, content) in EMBEDDED_RUNTIME_ASSETS { assert_eq!(std::fs::read_to_string(root.join(path)).expect("extracted asset"), *content); }
        assert!(root.join("kernels/js/worker-core.js").is_file());
        assert_eq!(extract_embedded_runtime_assets(base.path()).expect("re-extract"), root);
    }

    #[test]
    fn upstream_manifest_matches_every_embedded_asset() {
        use sha2::Digest;
        let manifest: serde_json::Value = serde_json::from_str(include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/UPSTREAM.json"))).expect("manifest json");
        let entries = manifest["assets"].as_array().expect("assets array");
        let shipped: std::collections::BTreeMap<&str, &serde_json::Value> = entries.iter()
            .filter(|entry| entry["status"] != "unported")
            .map(|entry| (entry["path"].as_str().expect("path"), entry)).collect();
        let embedded: std::collections::BTreeSet<&str> = EMBEDDED_RUNTIME_ASSETS.iter().map(|(path, _)| *path).collect();
        assert_eq!(shipped.keys().copied().collect::<std::collections::BTreeSet<_>>(), embedded, "UPSTREAM.json must list exactly the embedded assets");
        for (path, content) in EMBEDDED_RUNTIME_ASSETS {
            let entry = shipped[path];
            let digest = sha2::Sha256::digest(content.as_bytes()).iter().map(|byte| format!("{byte:02x}")).collect::<String>();
            assert_eq!(entry["sha256"], digest.as_str(), "{path} changed without updating UPSTREAM.json");
            match entry["status"].as_str() {
                Some("verbatim" | "patched") => assert!(entry["upstreamCommit"].as_str().is_some_and(|commit| commit.len() == 40), "{path} needs a full upstream commit"),
                Some("local") => assert!(entry["upstreamCommit"].is_null(), "{path} is mhc-only"),
                other => panic!("{path} has unknown status {other:?}"),
            }
        }
    }
}
