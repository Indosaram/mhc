use crate::shared::logger::log;
use std::path::{Path, PathBuf};
use utils::contains_path::is_within_project;

pub fn resolve_agent_definition_paths(
    paths: &[String],
    base_dir: &Path,
    containment_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut resolved_paths = Vec::new();

    for p in paths {
        let expanded = if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
            dirs::home_dir().unwrap_or_default().join(rest)
        } else if p == "~" {
            dirs::home_dir().unwrap_or_default()
        } else {
            PathBuf::from(p)
        };

        let resolved = if expanded.is_absolute() {
            expanded
        } else {
            base_dir.join(expanded)
        };

        if let Some(containment) = containment_dir
            && !is_within_project(&resolved, containment)
        {
            log(
                &format!(
                    "agent_definitions path rejected (outside project boundary): {p} -> {}",
                    resolved.display()
                ),
                None,
            );
            continue;
        }

        resolved_paths.push(resolved);
    }

    resolved_paths
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_tilde_and_resolves_relative_paths() {
        let root = tempfile::tempdir().expect("fixture directory");
        let base_dir = root.path().join("base");
        let project_dir = root.path().join("project");
        std::fs::create_dir_all(&base_dir).expect("base dir");
        std::fs::create_dir_all(&project_dir).expect("project dir");

        let paths = vec![
            "nested/agent.md".to_string(),
            project_dir.join("inside.md").to_string_lossy().into_owned(),
        ];

        let resolved = resolve_agent_definition_paths(&paths, &base_dir, None);
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0], base_dir.join("nested/agent.md"));
        assert_eq!(resolved[1], project_dir.join("inside.md"));
    }

    #[test]
    fn rejects_paths_outside_containment_directory() {
        let root = tempfile::tempdir().expect("fixture directory");
        let project_dir = root.path().join("project");
        let outside_dir = root.path().join("outside");
        std::fs::create_dir_all(&project_dir).expect("project dir");
        std::fs::create_dir_all(&outside_dir).expect("outside dir");

        let paths = vec![
            "agent.md".to_string(),
            outside_dir
                .join("escaped.md")
                .to_string_lossy()
                .into_owned(),
        ];

        let resolved = resolve_agent_definition_paths(&paths, &project_dir, Some(&project_dir));
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0], project_dir.join("agent.md"));
    }
}
