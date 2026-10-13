use std::path::{Path, PathBuf};

pub struct CodemodeRuntimeAssetEnvironment<'a> {
    pub bun_version: Option<&'a str>,
    pub executable_path: &'a Path,
}

#[derive(Debug, thiserror::Error)]
#[error("codemode runtime asset {package_relative_path} is unavailable: module-relative path {local_path} is not readable and the codemode sidecar is missing beside the executable {executable_path} (expected {sidecar_path}). Ship node_modules/@code-yeongyu/senpi-codemode next to the executable.")]
pub struct CodemodeRuntimeAssetMissingError {
    pub package_relative_path: PathBuf,
    pub local_path: PathBuf,
    pub sidecar_path: PathBuf,
    pub executable_path: PathBuf,
}

pub fn is_bun_virtual_path(path: &Path) -> bool {
    let path = path.to_string_lossy();
    path.contains("/$bunfs/") || path.contains("~BUN") || path.contains("%7EBUN")
}

fn sidecar_path_for(package_relative_path: &Path, executable_path: &Path) -> PathBuf {
    executable_path.parent().unwrap_or(Path::new(".")).join("node_modules/@code-yeongyu/senpi-codemode/src").join(package_relative_path)
}

pub fn require_codemode_runtime_asset(local_path: &Path, package_relative_path: &Path, environment: &CodemodeRuntimeAssetEnvironment<'_>) -> Result<PathBuf, CodemodeRuntimeAssetMissingError> {
    let sidecar_path = sidecar_path_for(package_relative_path, environment.executable_path);
    if !is_bun_virtual_path(local_path) && local_path.exists() { return Ok(local_path.into()); }
    if sidecar_path.exists() { return Ok(sidecar_path); }
    if let Some(embedded) = materialize_embedded_runtime_asset(package_relative_path) { return Ok(embedded); }
    Err(CodemodeRuntimeAssetMissingError {
        package_relative_path: package_relative_path.into(), local_path: local_path.into(),
        sidecar_path, executable_path: environment.executable_path.into(),
    })
}

pub fn resolve_codemode_runtime_asset(local_path: &Path, package_relative_path: &Path, environment: &CodemodeRuntimeAssetEnvironment<'_>) -> PathBuf {
    if local_path.exists() { return local_path.into(); }
    if environment.bun_version.is_some_and(|version| !version.is_empty()) {
        let sidecar = sidecar_path_for(package_relative_path, environment.executable_path);
        if sidecar.exists() { return sidecar; }
    }
    local_path.into()
}

/// Kernel scripts compiled into the binary, keyed by package-relative path. A distributed binary runs
/// away from the build tree and without the senpi-codemode sidecar, so these are extracted on demand.
const EMBEDDED_RUNTIME_ASSETS: &[(&str, &str)] = &[
    ("kernels/jl/prelude.jl", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/jl/prelude.jl"))),
    ("kernels/jl/runner.jl", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/jl/runner.jl"))),
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
    ("kernels/js/worker-core.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-core.js"))),
    ("kernels/js/worker-entry.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-entry.js"))),
    ("kernels/js/worker-indirect-eval.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-indirect-eval.js"))),
    ("kernels/js/worker-runtime.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-runtime.js"))),
    ("kernels/js/worker-shadow-guard.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-shadow-guard.js"))),
    ("kernels/js/worker-shell-capture.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/worker-shell-capture.js"))),
    ("kernels/js/workpool.js", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/js/workpool.js"))),
    ("kernels/py/prelude.py", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/py/prelude.py"))),
    ("kernels/rb/prelude.rb", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/rb/prelude.rb"))),
    ("kernels/rb/runner.rb", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/rb/runner.rb"))),
    ("kernels/rb/workpool.rb", include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/assets/kernels/rb/workpool.rb"))),
];

/// Writes every embedded kernel script once into a content-addressed directory (scripts import their
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
    fn missing_local_and_sidecar_fall_back_to_embedded_asset() {
        let environment = CodemodeRuntimeAssetEnvironment { bun_version: None, executable_path: Path::new("/absent/mhc") };
        let resolved = require_codemode_runtime_asset(Path::new("/absent/runner.rb"), Path::new("kernels/rb/runner.rb"), &environment).expect("embedded runner");
        assert!(resolved.ends_with("kernels/rb/runner.rb"));
        assert!(resolved.is_file());
        assert!(resolved.with_file_name("prelude.rb").is_file());
    }
}
