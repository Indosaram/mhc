use std::path::Path;

/// TS `isServerInstalled`: explicit paths are checked directly, then every PATH entry
/// (with PATHEXT suffixes on Windows). `node` is always considered installed.
pub fn is_server_installed(command: &[String]) -> bool {
    let Some(cmd) = command.first().filter(|cmd| !cmd.is_empty()) else {
        return false;
    };
    if (cmd.contains('/') || cmd.contains('\\')) && Path::new(cmd).exists() {
        return true;
    }
    let exts = executable_suffixes();
    let path_env = std::env::var_os("PATH")
        .or_else(|| cfg!(windows).then(|| std::env::var_os("Path")).flatten())
        .unwrap_or_default();
    for dir in std::env::split_paths(&path_env) {
        for suffix in &exts {
            if dir.join(format!("{cmd}{suffix}")).exists() {
                return true;
            }
        }
    }
    cmd == "node"
}

fn executable_suffixes() -> Vec<String> {
    if !cfg!(windows) {
        return vec![String::new()];
    }
    let defaults = ["", ".exe", ".cmd", ".bat", ".ps1"];
    let path_ext = std::env::var("PATHEXT").unwrap_or_default();
    if path_ext.is_empty() {
        return defaults.iter().map(|s| (*s).to_string()).collect();
    }
    let mut out: Vec<String> = vec![String::new()];
    for ext in path_ext
        .split(';')
        .filter(|ext| !ext.is_empty())
        .map(str::to_string)
        .chain(defaults[1..].iter().map(|s| (*s).to_string()))
    {
        if !out.contains(&ext) {
            out.push(ext);
        }
    }
    out
}
