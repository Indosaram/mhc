use std::path::{Path, PathBuf};
pub fn find_agents_md_up(start_dir: &Path, root_dir: &Path, file_names: &[String]) -> Vec<PathBuf> {
 let mut collected = Vec::new();
 let mut current = start_dir;
 loop {
  if current != root_dir && let Some(candidate) = file_names.iter().map(|name| current.join(name)).find(|path| path.exists()) { collected.push(candidate); }
  if current == root_dir { break; }
  let Some(parent) = current.parent() else { break; };
  if parent == current || !parent.starts_with(root_dir) { break; }
  current = parent;
 }
 collected.reverse(); collected
}
