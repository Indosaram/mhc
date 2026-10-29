//! Port of TS `post-edit/file-location.ts`.

use crate::lsp::outside_context_workspace::nearest_marked_ancestor;
use crate::request_context::{
    canonicalize_existing_or_nearest_ancestor, dirname, is_path_inside, resolve_from,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PostEditFileLocation {
    Project,
    OutsideProject,
    AgentConfig,
    Temp,
}

pub struct PostEditFileLocationOptions<'a> {
    pub cwd: &'a str,
    pub agent_dirs: &'a [String],
    pub temp_dirs: Option<&'a [String]>,
    pub home_dirs: Option<&'a [String]>,
}

pub fn classify_post_edit_file_location(
    file_path: &str,
    options: &PostEditFileLocationOptions<'_>,
) -> PostEditFileLocation {
    let resolved = resolve_from(options.cwd, file_path);
    let file = canonicalize_existing_or_nearest_ancestor(&resolved).unwrap_or(resolved);

    let canonical_agent_dirs = canonical_directories(options.agent_dirs);
    if canonical_agent_dirs.iter().any(|d| is_path_inside(d, &file)) {
        return PostEditFileLocation::AgentConfig;
    }

    let project_root = nearest_marked_ancestor(&dirname(&file));

    let default_temps = default_temp_directories();
    let temp_dirs = options.temp_dirs.unwrap_or(&default_temps);
    let canonical_temp_dirs = canonical_directories(temp_dirs);

    if let Some(temp_root) = canonical_temp_dirs.iter().find(|d| is_path_inside(d, &file)) {
        let own_project = match &project_root {
            Some(p) => p != temp_root && is_path_inside(temp_root, p),
            None => false,
        };
        return if own_project {
            PostEditFileLocation::Project
        } else {
            PostEditFileLocation::Temp
        };
    }

    let Some(root) = project_root else {
        return PostEditFileLocation::OutsideProject;
    };

    let default_homes = vec![std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| if cfg!(windows) { "C:\\Users\\default".to_string() } else { "/root".to_string() })];
    let home_dirs = options.home_dirs.unwrap_or(&default_homes);
    let canonical_home_dirs = canonical_directories(home_dirs);

    if canonical_home_dirs.contains(&root) {
        PostEditFileLocation::OutsideProject
    } else {
        PostEditFileLocation::Project
    }
}

fn default_temp_directories() -> Vec<String> {
    if cfg!(windows) {
        vec![std::env::temp_dir().to_string_lossy().into_owned()]
    } else {
        vec![
            std::env::temp_dir().to_string_lossy().into_owned(),
            "/tmp".to_string(),
        ]
    }
}

fn canonical_directories(dirs: &[String]) -> Vec<String> {
    dirs.iter()
        .map(|d| canonicalize_existing_or_nearest_ancestor(d).unwrap_or_else(|_| d.clone()))
        .collect()
}
