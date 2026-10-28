//! The bundled skill root (spec §2).

use std::path::{Path, PathBuf};

/// Environment variable naming the bundled skill root.
pub const SKILLS_ROOT_ENV: &str = "MAHO_SKILLS_ROOT";

/// The bundled skill root for this process: `$MAHO_SKILLS_ROOT` when it names an existing
/// directory, else `skills/` beside the running executable when it exists.
pub fn bundled_skills_root() -> Option<PathBuf> {
    let env = std::env::var_os(SKILLS_ROOT_ENV).map(PathBuf::from);
    let exe = std::env::current_exe().ok();
    bundled_skills_root_from(env.as_deref(), exe.as_deref())
}

/// [`bundled_skills_root`] with explicit inputs.
pub fn bundled_skills_root_from(env: Option<&Path>, exe: Option<&Path>) -> Option<PathBuf> {
    if let Some(root) = env.filter(|root| !root.as_os_str().is_empty())
        && root.is_dir()
    {
        return Some(root.to_path_buf());
    }
    let sibling = exe?.parent()?.join("skills");
    sibling.is_dir().then_some(sibling)
}
