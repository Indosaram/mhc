use std::path::{Path,PathBuf,Component};
pub fn resolve_patch_path(cwd:&Path,file_path:&Path)->PathBuf {
    let path=if file_path.is_absolute() { file_path.to_path_buf() } else { let cwd=if cwd.is_absolute() { cwd.to_path_buf() } else { std::env::current_dir().expect("process cwd").join(cwd) }; cwd.join(file_path) };
    let mut resolved=PathBuf::new();
    for part in path.components() { match part { Component::CurDir=>{},Component::ParentDir=>{resolved.pop();},other=>resolved.push(other.as_os_str()) } }
    resolved
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn relative_cwd_resolves_against_process_directory() { assert_eq!(resolve_patch_path(Path::new("relative"),Path::new("../file")),std::env::current_dir().unwrap().join("file")); }
    #[test] fn relative_parent_is_resolved() { assert_eq!(resolve_patch_path(Path::new("/project/src"),Path::new("../file")),PathBuf::from("/project/file")); }
    #[test] fn absolute_path_ignores_cwd() { assert_eq!(resolve_patch_path(Path::new("/project"),Path::new("/elsewhere/file")),PathBuf::from("/elsewhere/file")); }
}
