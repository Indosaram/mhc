//! Port of senpi `packages/coding-agent/src/nearest-parent-config.ts`.

use std::path::Path;

pub const MAX_PARENT_CONFIG_SEARCH_DEPTH: usize = 100;

fn is_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).map(|metadata| metadata.is_dir()).unwrap_or(false)
}

fn is_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).map(|metadata| metadata.is_file()).unwrap_or(false)
}

pub fn find_nearest_parent_config_dir(
    cwd: &str,
    home_dir: &str,
    config_dir_name: &str,
    required_child_dir: Option<&str>,
    required_child_file: Option<&str>,
) -> Option<String> {
    let normalized_home_dir = Path::new(home_dir);
    let mut current_dir = Path::new(cwd).to_path_buf();

    for _ in 0..=MAX_PARENT_CONFIG_SEARCH_DEPTH {
        if current_dir == normalized_home_dir {
            return None;
        }

        let config_dir = current_dir.join(config_dir_name);
        let has_required_child = match required_child_file {
            Some(required_child_file) => is_file(&config_dir.join(required_child_file)),
            None => required_child_dir.map(|required_child_dir| is_directory(&config_dir.join(required_child_dir))).unwrap_or(true),
        };
        if is_directory(&config_dir) && has_required_child {
            return Some(config_dir.to_string_lossy().into_owned());
        }

        let parent_dir = current_dir.parent()?.to_path_buf();
        if parent_dir == current_dir {
            return None;
        }
        current_dir = parent_dir;
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_nearest_project_config_dir_and_returns_the_config_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        let nested = project.join("a/b");
        std::fs::create_dir_all(project.join(".maho/agent")).expect("mkdir");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::create_dir_all(&home).expect("mkdir");
        let found = find_nearest_parent_config_dir(
            &nested.to_string_lossy(),
            &home.to_string_lossy(),
            ".maho",
            Some("agent"),
            None,
        );
        assert_eq!(found.as_deref(), Some(project.join(".maho").to_string_lossy().as_ref()));
    }

    #[test]
    fn stops_at_home_and_returns_none_without_a_match() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let nested = home.join("a/b");
        std::fs::create_dir_all(&nested).expect("mkdir");
        assert!(find_nearest_parent_config_dir(&nested.to_string_lossy(), &home.to_string_lossy(), ".maho", Some("agent"), None).is_none());
    }

    #[test]
    fn a_symlinked_config_dir_is_not_a_match() {
        let tmp = crate::test_support::isolated_tempdir();
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        let nested = project.join("a");
        std::fs::create_dir_all(&home).expect("mkdir");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::create_dir_all(home.join("real/.maho/agent")).expect("mkdir");
        std::os::unix::fs::symlink(home.join("real/.maho"), project.join(".maho")).expect("symlink");
        assert!(find_nearest_parent_config_dir(&nested.to_string_lossy(), &home.to_string_lossy(), ".maho", Some("agent"), None).is_none());
    }

    #[test]
    fn a_flat_layout_needs_the_sentinel_file() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let project = tmp.path().join("proj");
        let nested = project.join("a");
        std::fs::create_dir_all(project.join(".maho")).expect("mkdir");
        std::fs::create_dir_all(&nested).expect("mkdir");
        std::fs::create_dir_all(&home).expect("mkdir");
        assert!(find_nearest_parent_config_dir(&nested.to_string_lossy(), &home.to_string_lossy(), ".maho", None, Some("settings.json")).is_none());
        std::fs::write(project.join(".maho/settings.json"), "{}").expect("write");
        assert_eq!(
            find_nearest_parent_config_dir(&nested.to_string_lossy(), &home.to_string_lossy(), ".maho", None, Some("settings.json")).as_deref(),
            Some(project.join(".maho").to_string_lossy().as_ref())
        );
    }
}
