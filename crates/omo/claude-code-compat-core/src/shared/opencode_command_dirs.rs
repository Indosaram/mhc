use super::opencode_config_dir::{OpenCodeConfigDirOptions, get_open_code_config_dirs};
use std::path::PathBuf;

pub fn get_open_code_command_dirs(options: &OpenCodeConfigDirOptions) -> Vec<PathBuf> {
    command_dirs_for_configs(&get_open_code_config_dirs(options))
}

fn command_dirs_for_configs(config_dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut result = Vec::new();
    for config_dir in config_dirs {
        let mut roots = vec![config_dir.as_path()];
        if let Some(profiles) = config_dir
            .parent()
            .filter(|p| p.file_name().is_some_and(|n| n == "profiles"))
            && let Some(parent) = profiles.parent()
        {
            roots.push(parent);
        }
        for root in roots {
            for name in ["commands", "command"] {
                let dir = root.join(name);
                if !result.contains(&dir) {
                    result.push(dir);
                }
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn profiles_precede_parent_aliases_and_duplicate_roots_are_removed() {
        // Given a profile and its parent as config roots.
        let roots = [
            PathBuf::from("/config/opencode/profiles/work"),
            PathBuf::from("/config/opencode"),
        ];
        // When expanding command aliases.
        let result = command_dirs_for_configs(&roots);
        // Then profile directories precede deduplicated parent directories.
        assert_eq!(
            result,
            [
                "/config/opencode/profiles/work/commands",
                "/config/opencode/profiles/work/command",
                "/config/opencode/commands",
                "/config/opencode/command"
            ]
            .map(PathBuf::from)
        );
    }
}
