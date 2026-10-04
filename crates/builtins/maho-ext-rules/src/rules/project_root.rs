use std::path::{Path, PathBuf};
use super::constants::PROJECT_MARKERS;
pub fn find_project_root(start_path: &Path, markers: Option<&[&str]>) -> Option<PathBuf> {
 let resolved = std::path::absolute(start_path).ok()?;
 let metadata = resolved.metadata().ok()?;
 let mut current = if metadata.is_dir() { resolved.as_path() } else { resolved.parent()? };
 loop {
  if markers.unwrap_or(PROJECT_MARKERS).iter().any(|marker| current.join(marker).exists()) { return Some(current.to_path_buf()); }
  let parent = current.parent()?;
  if parent == current { return None; }
  current = parent;
 }
}
