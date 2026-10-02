use std::path::{Path, PathBuf};

pub fn resolve_and_contain(file_path: &Path, root_dir: &Path) -> Option<(PathBuf, PathBuf)> {
    if file_path.as_os_str().is_empty() { return None; }
    let resolved = if file_path.is_absolute() { file_path.to_owned() } else { root_dir.join(file_path) };
    let root = root_dir.canonicalize().ok()?;
    let path = resolved.canonicalize().ok()?;
    if path == root || !path.starts_with(&root) { return None; }
    Some((path, root))
}
