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
        return executable(if path.is_absolute() { path.into() } else { cwd.join(path) });
    }
    let path = env.get("PATH").or_else(|| env.get("Path"))?;
    for directory in path.split(if windows { ';' } else { ':' }).filter(|s| !s.is_empty()) {
        if let Some(found) = executable(Path::new(directory).join(command)) { return Some(found); }
    }
    None
}
