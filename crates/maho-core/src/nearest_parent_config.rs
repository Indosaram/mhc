//! Port of senpi `packages/coding-agent/src/nearest-parent-config.ts`.

use std::path::Path;

/// Walks up from `cwd` (stopping at `home_dir`) looking for `<dir>/<config_dir>`; when `segment`
/// is given the candidate must be `<dir>/<config_dir>/<segment>`, and `sentinel` requires a file
/// inside it.
pub fn find_nearest_parent_config_dir(
    cwd: &str,
    home_dir: &str,
    config_dir: &str,
    segment: Option<&str>,
    sentinel: Option<&str>,
) -> Option<String> {
    let home = Path::new(home_dir);
    let mut current = Path::new(cwd).to_path_buf();
    loop {
        let candidate = match segment {
            Some(segment) => current.join(config_dir).join(segment),
            None => current.join(config_dir),
        };
        if candidate.is_dir() {
            let ok = match sentinel {
                Some(sentinel) => candidate.join(sentinel).is_file(),
                None => true,
            };
            if ok {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
        if current == home || current.parent().is_none() {
            return None;
        }
        let parent = current.parent()?.to_path_buf();
        if parent == current {
            return None;
        }
        current = parent;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_nearest_project_config_dir() {
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
        assert_eq!(found.as_deref(), Some(project.join(".maho/agent").to_string_lossy().as_ref()));
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
    fn sentinel_is_required_for_a_flat_layout_match() {
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
