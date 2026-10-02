pub use maho_core::paths::*;
use std::{collections::VecDeque, path::{Path, PathBuf}};
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
