pub use maho_core::config::*;
use std::path::Path;
pub fn get_bin_dir() -> String { Path::new(&get_agent_dir()).join("bin").to_string_lossy().into_owned() }
pub fn get_tools_dir() -> String { Path::new(&get_agent_dir()).join("tools").to_string_lossy().into_owned() }
pub fn get_prompts_dir() -> String { Path::new(&get_agent_dir()).join("prompts").to_string_lossy().into_owned() }
pub fn get_custom_themes_dir() -> String { Path::new(&get_agent_dir()).join("themes").to_string_lossy().into_owned() }
pub fn get_changelog_path() -> String { Path::new(&get_package_dir()).join("CHANGELOG.md").to_string_lossy().into_owned() }
pub fn get_share_viewer_url(gist_id: &str) -> String { let base = maho_core::brand::env_value("SHARE_VIEWER_URL", &current_env()).unwrap_or_else(|| "https://pi.dev/session/".to_owned()); format!("{base}#{gist_id}") }
pub fn resolve_shipped_asset_dir(preferred: &Path, installed: &Path, directory: &str, probe: &str) -> std::path::PathBuf {
    let path = preferred.join(directory);
    if path.join(probe).exists() { path } else { installed.join(directory) }
}
fn shipped_asset(directory: &str, probe: &str) -> String {
    let installed = std::env::current_exe().ok().and_then(|executable| executable.parent().map(Path::to_path_buf)).unwrap_or_default();
    resolve_shipped_asset_dir(Path::new(&get_package_dir()), &installed, directory, probe).to_string_lossy().into_owned()
}
pub fn get_themes_dir() -> String { shipped_asset("theme", "dark.json") }
pub fn get_export_template_dir() -> String { shipped_asset("export-html", "template.html") }
pub fn get_interactive_assets_dir() -> String { shipped_asset("assets", "clankolas.png") }
pub fn get_bundled_interactive_asset_path(name: &str) -> String { Path::new(&get_interactive_assets_dir()).join(name).to_string_lossy().into_owned() }
