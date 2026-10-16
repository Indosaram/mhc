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

pub fn widen_to_repository_root(project_root: Option<PathBuf>, home_dir: &Path) -> Option<PathBuf> {
 let project_root = project_root?;
 let home = std::fs::canonicalize(home_dir).unwrap_or_else(|_| super::finder::absolute(home_dir));
 let mut current = std::fs::canonicalize(&project_root).unwrap_or_else(|_| super::finder::absolute(&project_root));
 loop {
  if current == home { return Some(project_root); }
  if current.join(".git").exists() { return Some(current); }
  let parent = current.parent()?;
  if parent == current { return Some(project_root); }
  current = parent.to_path_buf();
 }
}

#[cfg(test)]
mod tests {
 use super::*;
 use std::fs;

 #[test]
 fn widens_nested_project_to_repository_but_not_home_dotfiles_repository() {
  let base = std::env::temp_dir().join(format!("maho-rules-root-{}", std::process::id()));
  let home = base.join("home");
  let repository = base.join("repo");
  let nested = repository.join("crates/member");
  fs::create_dir_all(&nested).expect("create nested project");
  fs::create_dir_all(home.join("project")).expect("create home project");
  fs::create_dir_all(repository.join(".git")).expect("create repository marker");
  fs::create_dir_all(home.join(".git")).expect("create home repository marker");
  assert_eq!(widen_to_repository_root(Some(nested), &home), Some(repository));
  let home_project = home.join("project");
  assert_eq!(widen_to_repository_root(Some(home_project.clone()), &home), Some(home_project));
  let _ = fs::remove_dir_all(base);
 }
}
