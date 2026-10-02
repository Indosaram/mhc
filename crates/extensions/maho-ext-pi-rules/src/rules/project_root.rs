use std::path::{Path, PathBuf};
use super::constants::PROJECT_MARKERS;

pub fn find_project_root(start_path: &Path, markers: Option<&[&str]>) -> Option<PathBuf> {
    let resolved = start_path.canonicalize().ok()?;
    let metadata = resolved.metadata().ok()?;
    let mut current = if metadata.is_dir() { resolved } else { resolved.parent()?.to_owned() };
    loop {
        if markers.unwrap_or(PROJECT_MARKERS).iter().any(|marker| current.join(marker).exists()) { return Some(current); }
        current = current.parent()?.to_owned();
    }
}
