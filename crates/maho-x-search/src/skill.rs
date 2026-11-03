//! The bundled `x-search` usage skill (spec §6).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The skill name (interface fact).
pub const SKILL_NAME: &str = "x-search";
/// The skill text shipped inside the binary.
pub const SKILL_TEXT: &str = include_str!("../skill/x-search/SKILL.md");

/// The per-user folder the embedded skill is materialized in:
/// `<agent_dir>/cache/x-search-<version>/skill`. Never a shared temp dir, where another local user
/// could pre-create the folder (or a symlink) and control the skill text.
pub fn embedded_skill_dir(agent_dir: &Path) -> PathBuf {
    agent_dir
        .join("cache")
        .join(format!("x-search-{}", env!("CARGO_PKG_VERSION")))
        .join("skill")
}

/// Writes the embedded skill once into the owner-only [`embedded_skill_dir`] (a run away from the
/// build tree has no source copy) and returns its path; `None` when it cannot be written or a
/// symlink stands in place of either folder.
pub fn embedded_skill_path(agent_dir: &Path) -> Option<PathBuf> {
    let dir = embedded_skill_dir(agent_dir);
    let version_dir = dir.parent()?;
    let path = dir.join("SKILL.md");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(SKILL_TEXT) {
        std::fs::create_dir_all(version_dir.parent()?).ok()?;
        maho_ext_api::private_dir::create_private_dir(version_dir)?;
        maho_ext_api::private_dir::create_private_dir(&dir)?;
        // Write beside the target and rename, so a concurrent reader never sees a partial file.
        let staging = dir.join(format!(".SKILL.md.{}.tmp", std::process::id()));
        std::fs::write(&staging, SKILL_TEXT).ok()?;
        std::fs::rename(&staging, &path).ok()?;
    }
    Some(path)
}

/// Warning sink for a missing skill file.
pub type WarnSink = Arc<dyn Fn(&str) + Send + Sync>;

/// Resolves the skill file to contribute. A missing file is warned about once per `warned` flag.
pub fn resolve_skill_file(path: &Path, warned: &AtomicBool, warn: &WarnSink) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if !warned.swap(true, Ordering::SeqCst) {
        warn(&format!(
            "x-search: skill file not found at {}; the x_search tool is available without its skill",
            path.display()
        ));
    }
    None
}
