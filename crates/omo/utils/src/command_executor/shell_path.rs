use std::path::Path;

const DEFAULT_ZSH_PATHS: [&str; 3] = ["/bin/zsh", "/usr/bin/zsh", "/usr/local/bin/zsh"];
const DEFAULT_BASH_PATHS: [&str; 3] = ["/bin/bash", "/usr/bin/bash", "/usr/local/bin/bash"];

fn find_shell_path(default_paths: &[&str], custom_path: Option<&str>) -> Option<String> {
    custom_path
        .filter(|path| Path::new(path).exists())
        .or_else(|| {
            default_paths
                .iter()
                .copied()
                .find(|path| Path::new(path).exists())
        })
        .map(str::to_string)
}

pub fn find_zsh_path(custom_zsh_path: Option<&str>) -> Option<String> {
    find_shell_path(&DEFAULT_ZSH_PATHS, custom_zsh_path)
}

pub fn find_bash_path() -> Option<String> {
    find_shell_path(&DEFAULT_BASH_PATHS, None)
}
