pub use maho_core::config::*;
use std::path::Path;
pub fn get_bin_dir() -> String { Path::new(&get_agent_dir()).join("bin").to_string_lossy().into_owned() }
pub fn get_tools_dir() -> String { Path::new(&get_agent_dir()).join("tools").to_string_lossy().into_owned() }
pub fn get_prompts_dir() -> String { Path::new(&get_agent_dir()).join("prompts").to_string_lossy().into_owned() }
pub fn get_custom_themes_dir() -> String { Path::new(&get_agent_dir()).join("themes").to_string_lossy().into_owned() }
pub fn get_changelog_path() -> String { Path::new(&get_package_dir()).join("CHANGELOG.md").to_string_lossy().into_owned() }
pub fn get_share_viewer_url(gist_id: &str) -> String { let base = maho_core::brand::env_value("SHARE_VIEWER_URL", &current_env()).unwrap_or_else(|| "https://pi.dev/session/".to_owned()); format!("{base}#{gist_id}") }
