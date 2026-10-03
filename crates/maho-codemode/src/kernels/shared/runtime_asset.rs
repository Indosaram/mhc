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
