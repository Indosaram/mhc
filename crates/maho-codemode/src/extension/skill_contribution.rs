use crate::kernels::shared::runtime_asset::materialize_embedded_runtime_asset;
use maho_ext_api::{diagnostic, EventKind, EventResult, ExtensionApi, ResourcesDiscoverResult};
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

pub fn bundled_bun_skill_path(base_dir: &Path) -> Option<PathBuf> {
    let local = base_dir.join("../skill/bun-1-4/SKILL.md");
    if local.exists() { return Some(local); }
    // A distributed binary runs away from the build tree; the skill tree is embedded beside the
    // kernel scripts, as `require_codemode_runtime_asset` does for kernels.
    if let Some(embedded) = materialize_embedded_runtime_asset(Path::new("skill/bun-1-4/SKILL.md")) && embedded.exists() { return Some(embedded); }
    if !LOGGED_MISSING_BUN_SKILL.swap(true, Ordering::Relaxed) {
        diagnostic(&format!("[senpi-codemode] bundled bun-1-4 skill not found at {} and the embedded copy could not be extracted; skipping contribution", local.display()));
    }
    None
}

pub fn active_bun_skill_path(version: Option<&str>, base_dir: &Path) -> Option<PathBuf> {
    if !bun_version_supports_skill(version) { return None; }
    bundled_bun_skill_path(base_dir)
}

pub fn register_bun_skill_contribution(api: &mut ExtensionApi, version: Arc<dyn Fn() -> Option<String> + Send + Sync>, base_dir: PathBuf) {
    api.on(EventKind::ResourcesDiscover, Arc::new(move |_, _| {
        let version = version();
        let path = active_bun_skill_path(version.as_deref(), &base_dir);
        Box::pin(async move {
            Ok(match path {
                Some(path) => EventResult::ResourcesDiscover(ResourcesDiscoverResult { skill_paths: vec![path.to_string_lossy().into_owned().into()], ..Default::default() }),
                None => EventResult::None,
            })
        })
    }));
}
