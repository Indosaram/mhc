//! The bundled `x-search` usage skill (spec §6).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The skill name (interface fact).
pub const SKILL_NAME: &str = "x-search";
/// The skill text shipped inside the binary.
pub const SKILL_TEXT: &str = include_str!("../skill/x-search/SKILL.md");

/// Writes the embedded skill once under the temp dir (a run away from the build tree has no
/// source copy) and returns its path; `None` when it cannot be written.
pub fn embedded_skill_path() -> Option<PathBuf> {
    let path = std::env::temp_dir()
        .join(format!("maho-x-search-{}", env!("CARGO_PKG_VERSION")))
        .join(SKILL_NAME)
        .join("SKILL.md");
    if std::fs::read_to_string(&path).ok().as_deref() != Some(SKILL_TEXT) {
        let dir = path.parent()?;
        std::fs::create_dir_all(dir).ok()?;
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
