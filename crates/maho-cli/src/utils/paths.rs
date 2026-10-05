pub use maho_core::paths::*;
use std::{collections::VecDeque, path::{Path, PathBuf}};
#[derive(Clone)]
pub struct PathInputOptions { pub trim: bool, pub expand_tilde: bool, pub home_dir: Option<String>, pub strip_at_prefix: bool, pub normalize_unicode_spaces: bool }
impl Default for PathInputOptions { fn default() -> Self { Self { trim: false, expand_tilde: true, home_dir: None, strip_at_prefix: false, normalize_unicode_spaces: false } } }
pub fn normalize_path(input: &str, options: &PathInputOptions) -> Result<String, String> {
    let mut normalized = if options.trim { input.trim().to_owned() } else { input.to_owned() };
    if options.normalize_unicode_spaces { normalized = normalized.chars().map(|character| if matches!(character, '\u{a0}' | '\u{2000}'..='\u{200a}' | '\u{202f}' | '\u{205f}' | '\u{3000}') { ' ' } else { character }).collect(); }
    if options.strip_at_prefix && normalized.starts_with('@') { normalized.remove(0); }
    if cfg!(windows) { normalized = normalize_windows_shell_path(&normalized); }
    if options.expand_tilde {
        let home = options.home_dir.clone().unwrap_or_else(maho_core::config::home_dir);
        if normalized == "~" { return Ok(home); }
        if let Some(rest) = normalized.strip_prefix("~/").or_else(|| if cfg!(windows) { normalized.strip_prefix("~\\") } else { None }) { return Ok(Path::new(&home).join(rest).to_string_lossy().into_owned()); }
    }
    if normalized.starts_with("file://") {
        return url::Url::parse(&normalized).map_err(|error| error.to_string())?.to_file_path().map(|path| path.to_string_lossy().into_owned()).map_err(|()| "Invalid file URL path".to_owned());
    }
    Ok(normalized)
}
pub fn resolve_path(input: &str, base: &str, options: &PathInputOptions) -> Result<String, String> {
    let normalized = normalize_path(input, options)?;
    let base = normalize_path(base, &PathInputOptions::default())?;
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let absolute = cwd.join(base).join(normalized);
    let mut path = PathBuf::new();
    for component in absolute.components() { match component { std::path::Component::CurDir => {}, std::path::Component::ParentDir => { path.pop(); }, component => path.push(component.as_os_str()) } }
    Ok(path.to_string_lossy().into_owned())
}
pub fn get_cwd_relative_path(file: &str, cwd: &str) -> Result<Option<String>, String> {
    let cwd = resolve_path(cwd, cwd, &Default::default())?;
    let file = resolve_path(file, &cwd, &Default::default())?;
    Ok(Path::new(&file).strip_prefix(&cwd).ok().map(|relative| if relative.as_os_str().is_empty() { ".".to_owned() } else { relative.to_string_lossy().into_owned() }))
}
pub fn format_path_relative_to_cwd_or_absolute(file: &str, cwd: &str) -> Result<String, String> {
    let absolute = resolve_path(file, cwd, &Default::default())?;
    let path = get_cwd_relative_path(&absolute, cwd)?.unwrap_or(absolute);
    Ok(if cfg!(windows) { path.replace('\\', "/") } else { path })
}
pub fn mark_path_ignored_by_cloud_sync(path: &str) {
    let attrs: &[&str] = if cfg!(target_os = "macos") { &["com.dropbox.ignored", "com.apple.fileprovider.ignore#P"] } else if cfg!(target_os = "linux") { &["user.com.dropbox.ignored"] } else { &[] };
    for attr in attrs {
        let mut command = std::process::Command::new(if cfg!(target_os = "macos") { "xattr" } else { "setfattr" });
        if cfg!(target_os = "macos") { command.args(["-w", attr, "1", path]); } else { command.args(["-n", attr, "-v", "1", path]); }
        let _ = command.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
    }
}
#[cfg(unix)]
pub fn get_file_revision(path: &str) -> Option<String> {
    use std::os::unix::fs::MetadataExt;
    let metadata = std::fs::metadata(path).ok()?;
    let mtime = i128::from(metadata.mtime()) * 1_000_000_000 + i128::from(metadata.mtime_nsec());
    let ctime = i128::from(metadata.ctime()) * 1_000_000_000 + i128::from(metadata.ctime_nsec());
    Some(format!("{}:{}:{}:{mtime}:{ctime}", metadata.dev(), metadata.ino(), metadata.len()))
}
fn resolve_without_open(input: &str) -> (String, Option<std::io::Error>) {
    let absolute = if Path::new(input).is_absolute() { PathBuf::from(input) } else {
        match std::env::current_dir() { Ok(cwd) => cwd.join(input), Err(error) => return (input.to_owned(), Some(error)) }
    };
    let mut pending: VecDeque<_> = absolute.components().map(|part| part.as_os_str().to_owned()).collect();
    let mut resolved = PathBuf::new();
    let mut hops = 0;
    while let Some(part) = pending.pop_front() {
        if part == "." { continue; }
        if part == ".." { resolved.pop(); continue; }
        let candidate = resolved.join(&part);
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                hops += 1;
                if hops > 40 {
                    let mut path = candidate;
                    path.extend(pending);
                    return (path.to_string_lossy().into_owned(), Some(std::io::Error::from_raw_os_error(40)));
                }
                match std::fs::read_link(&candidate) {
                    Ok(target) => {
                        if target.is_absolute() { resolved.clear(); }
                        let mut parts: VecDeque<_> = target.components().map(|part| part.as_os_str().to_owned()).collect();
                        parts.append(&mut pending);
                        pending = parts;
                    }
                    Err(error) => {
                        let mut path = candidate;
                        path.extend(pending);
                        return (path.to_string_lossy().into_owned(), Some(error));
                    }
                }
            }
            Ok(_) => resolved.push(part),
            Err(error) => {
                let mut path = candidate;
                path.extend(pending);
                return (path.to_string_lossy().into_owned(), Some(error));
            }
        }
    }
    (resolved.to_string_lossy().into_owned(), None)
}
pub fn realpath_without_open(input: &str) -> String { resolve_without_open(input).0 }
pub fn realpath_without_open_strict(input: &str) -> std::io::Result<String> {
    let (path, error) = resolve_without_open(input);
    if let Some(error) = error && !matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) { return Err(error); }
    Ok(path)
}
