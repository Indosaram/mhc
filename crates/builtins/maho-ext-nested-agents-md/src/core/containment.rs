use std::path::{Path, PathBuf};
pub struct ContainmentResult { pub canonical_path: PathBuf, pub canonical_root: PathBuf }
pub fn resolve_and_contain(file_path: &Path, root_dir: &Path) -> Option<ContainmentResult> {
 if file_path.as_os_str().is_empty() { return None; }
 let resolved = if file_path.is_absolute() { file_path.to_path_buf() } else { root_dir.join(file_path) };
 let canonical_root = root_dir.canonicalize().ok()?;
 let canonical_path = resolved.canonicalize().ok()?;
 if canonical_path == canonical_root || !canonical_path.starts_with(&canonical_root) { return None; }
 Some(ContainmentResult { canonical_path, canonical_root })
}
