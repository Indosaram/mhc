use crate::kernels::shared::runtime_asset::{CodemodeRuntimeAssetEnvironment, resolve_codemode_runtime_asset};
use maho_ext_api::{EventKind, EventResult, ExtensionApi, ResourcesDiscoverResult};
use std::path::{Path, PathBuf};
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

pub const BUN_SKILL_MIN_VERSION: &str = "1.4.0";
static LOGGED_MISSING_BUN_SKILL: AtomicBool = AtomicBool::new(false);

pub fn bun_version_supports_skill(version: Option<&str>) -> bool {
    let Some(version) = version else { return false; };
    let Some((major, rest)) = version.split_once('.') else { return false; };
    let minor: String = rest.chars().take_while(char::is_ascii_digit).collect();
    match (major.parse::<u64>(), minor.parse::<u64>()) {
        (Ok(major), Ok(minor)) => major > 1 || (major == 1 && minor >= 4),
        _ => false,
    }
}

pub fn bundled_bun_skill_path(base_dir: &Path, environment: &CodemodeRuntimeAssetEnvironment<'_>) -> Option<PathBuf> {
    let local = base_dir.join("../skill/bun-1-4/SKILL.md");
    let relative = Path::new("skill/bun-1-4/SKILL.md");
    let candidate = resolve_codemode_runtime_asset(&local, relative, environment);
    if candidate.exists() { return Some(candidate); }
    if !LOGGED_MISSING_BUN_SKILL.swap(true, Ordering::Relaxed) {
        let sidecar = environment.executable_path.parent().unwrap_or(Path::new(".")).join("node_modules/@code-yeongyu/senpi-codemode/src").join(relative);
        eprintln!("[senpi-codemode] bundled bun-1-4 skill not found at {} or {}; skipping contribution", local.display(), sidecar.display());
    }
    None
}

pub fn active_bun_skill_path(version: Option<&str>, base_dir: &Path, executable_path: &Path) -> Option<PathBuf> {
    if !bun_version_supports_skill(version) { return None; }
    bundled_bun_skill_path(base_dir, &CodemodeRuntimeAssetEnvironment { bun_version: version, executable_path })
}

pub fn register_bun_skill_contribution(api: &mut ExtensionApi, version: Arc<dyn Fn() -> Option<String> + Send + Sync>, base_dir: PathBuf, executable_path: PathBuf) {
    api.on(EventKind::ResourcesDiscover, Arc::new(move |_, _| {
        let version = version();
        let path = active_bun_skill_path(version.as_deref(), &base_dir, &executable_path);
        Box::pin(async move {
            Ok(match path {
                Some(path) => EventResult::ResourcesDiscover(ResourcesDiscoverResult { skill_paths: vec![path.to_string_lossy().into_owned().into()], ..Default::default() }),
                None => EventResult::None,
            })
        })
    }));
}
