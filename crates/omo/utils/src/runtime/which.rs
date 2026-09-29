use std::path::{Path, PathBuf};

fn is_unsafe_command_name(name: &str) -> bool {
    name.contains('/')
        || name.contains('\\')
        || name == "."
        || name.contains("..")
        || name.contains('\0')
        || (name.len() >= 2
            && name.as_bytes()[0].is_ascii_alphabetic()
            && name.as_bytes()[1] == b':')
}

fn candidate_names(name: &str) -> Vec<String> {
    if !cfg!(windows) || Path::new(name).extension().is_some() {
        return vec![name.to_string()];
    }
    ["", ".exe", ".cmd", ".bat", ".com"]
        .iter()
        .map(|ext| format!("{name}{ext}"))
        .collect()
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// Resolve a bare command name on PATH; names containing separators or traversal are rejected.
pub fn bun_which(command_name: &str) -> Option<PathBuf> {
    if command_name.is_empty() || is_unsafe_command_name(command_name) {
        return None;
    }
    let path_value = std::env::var_os("PATH").or_else(|| std::env::var_os("Path"))?;
    let names = candidate_names(command_name);
    std::env::split_paths(&path_value)
        .filter(|entry| !entry.as_os_str().is_empty())
        .flat_map(|entry| names.iter().map(move |name| entry.join(name)))
        .find(|candidate| is_executable(candidate))
}
