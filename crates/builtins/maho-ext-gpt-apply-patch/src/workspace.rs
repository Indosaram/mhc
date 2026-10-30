use std::path::{Path,PathBuf,Component};
#[cfg(windows)]
pub fn resolve_patch_path(cwd:&Path,file_path:&Path)->PathBuf {
    resolve_windows_patch_path(cwd,file_path)
}
#[cfg(not(windows))]
pub fn resolve_patch_path(cwd:&Path,file_path:&Path)->PathBuf {
    let path=if file_path.is_absolute() { file_path.to_path_buf() } else { let cwd=if cwd.is_absolute() { cwd.to_path_buf() } else { std::env::current_dir().unwrap_or_default().join(cwd) }; cwd.join(file_path) };
    let mut resolved=PathBuf::new();
    for part in path.components() { match part { Component::CurDir=>{},Component::ParentDir=>{resolved.pop();},other=>resolved.push(other.as_os_str()) } }
    resolved
}
#[cfg(windows)]
fn resolve_windows_patch_path(cwd:&Path,file_path:&Path)->PathBuf {
    let absolute=if file_path.is_absolute() {file_path.to_path_buf()} else {let base=if cwd.is_absolute(){cwd.to_path_buf()}else{std::env::current_dir().unwrap_or_default().join(cwd)};base.join(file_path)};
    let mut resolved=PathBuf::new();
    for component in absolute.components(){match component{Component::Prefix(prefix)=>resolved.push(prefix.as_os_str()),Component::RootDir=>resolved.push(component.as_os_str()),Component::CurDir=>{},Component::ParentDir=>{if resolved.file_name().is_some(){resolved.pop();}},Component::Normal(part)=>resolved.push(part)}}
    resolved
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn relative_cwd_resolves_against_process_directory() { assert_eq!(resolve_patch_path(Path::new("relative"),Path::new("../file")),std::env::current_dir().unwrap().join("file")); }
    #[test] fn relative_parent_is_resolved() {
        let (project, file) = if cfg!(windows) { (Path::new(r"C:\project\src"), PathBuf::from(r"C:\project\file")) } else { (Path::new("/project/src"), PathBuf::from("/project/file")) };
        assert_eq!(resolve_patch_path(project, Path::new("../file")), file);
    }
    #[test] fn absolute_path_ignores_cwd() {
        let (project, elsewhere) = if cfg!(windows) { (Path::new(r"C:\project"), PathBuf::from(r"C:\elsewhere\file")) } else { (Path::new("/project"), PathBuf::from("/elsewhere/file")) };
        assert_eq!(resolve_patch_path(project, &elsewhere), elsewhere);
    }
    #[cfg(windows)]
    #[test] fn windows_resolution_preserves_drive_prefix_and_clamps_parent_at_root() { assert_eq!(resolve_patch_path(Path::new(r"C:\project\src"),Path::new(r"..\..\file")),PathBuf::from(r"C:\file")); }
}
