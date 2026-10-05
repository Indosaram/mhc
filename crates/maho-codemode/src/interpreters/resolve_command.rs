use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub fn resolve_command_path(command: &str, env: &HashMap<String, String>, cwd: &Path, windows: bool) -> Option<PathBuf> {
    if command.is_empty() { return None; }
    let executable = |base: PathBuf| {
        let mut candidates = vec![base.clone()];
        if windows {
            for extension in env.get("PATHEXT").map(String::as_str).unwrap_or(".COM;.EXE;.BAT;.CMD").split(';').map(str::trim).filter(|s| s.starts_with('.')) {
                candidates.push(format!("{}{}", base.display(), extension.to_lowercase()).into());
                candidates.push(format!("{}{extension}", base.display()).into());
            }
        }
        candidates.into_iter().find(|candidate| {
            let Ok(metadata) = std::fs::metadata(candidate) else { return false; };
            if !metadata.is_file() { return false; }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                windows || metadata.permissions().mode() & 0o111 != 0
            }
            #[cfg(not(unix))]
            { true }
        })
    };
    if command.contains('/') || (windows && command.contains('\\')) {
        let path = Path::new(command);
        let absolute = if path.is_absolute() { path.into() } else {
            let mut resolved = PathBuf::new();
            for component in cwd.join(path).components() {
                match component {
                    std::path::Component::CurDir => {},
                    std::path::Component::ParentDir => { resolved.pop(); },
                    component => resolved.push(component.as_os_str()),
                }
            }
            resolved
        };
        return executable(absolute);
    }
    let path = env.get("PATH").or_else(|| env.get("Path"))?;
    for directory in path.split(if cfg!(windows) { ';' } else { ':' }).filter(|s| !s.is_empty()) {
        let separator=if directory.ends_with(['/', '\\']) {""} else if windows && directory.contains('\\') && !directory.contains('/') {"\\"} else {"/"};
        if let Some(found) = executable(format!("{directory}{separator}{command}").into()) { return Some(found); }
    }
    None
}
