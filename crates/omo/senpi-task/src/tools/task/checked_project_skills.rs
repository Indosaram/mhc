//! Provenance-checked project skill discovery, mirroring checked-project-skills.ts.
use super::skills::{frontmatter_name, skill_body};
use super::types::LoadedSkill;
use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum SkillReadError {
    Blocked { path: PathBuf, target: PathBuf },
    Io { path: PathBuf, error: io::Error },
}
impl std::fmt::Display for SkillReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Blocked { path, target } => write!(
                f,
                "Blocked skill link outside the project: {} -> {}. Add the target folder to your user skills settings to allow it.",
                path.display(),
                target.display()
            ),
            Self::Io { error, .. } => std::fmt::Display::fmt(error, f),
        }
    }
}
impl std::error::Error for SkillReadError {}
impl SkillReadError {
    pub fn path(&self) -> &Path {
        match self {
            Self::Blocked { path, .. } | Self::Io { path, .. } => path,
        }
    }
}
#[derive(Default)]
pub struct CheckedProjectSkills {
    pub skills: HashMap<String, LoadedSkill>,
    pub blocked: Vec<SkillReadError>,
    pub errors: Vec<SkillReadError>,
}

pub fn check_skill_path(
    path: &Path,
    project: &Path,
    user_roots: &[PathBuf],
) -> Result<PathBuf, SkillReadError> {
    let canonical = fs::canonicalize(path).map_err(|error| SkillReadError::Io {
        path: path.to_path_buf(),
        error,
    })?;
    let root = fs::canonicalize(project).map_err(|error| SkillReadError::Io {
        path: project.to_path_buf(),
        error,
    })?;
    if canonical.starts_with(root)
        || user_roots
            .iter()
            .any(|root| user_allows(root, &canonical, &mut HashSet::new(), true))
    {
        return Ok(canonical);
    }
    Err(SkillReadError::Blocked {
        path: path.to_path_buf(),
        target: canonical,
    })
}
fn user_allows(path: &Path, target: &Path, ancestry: &mut HashSet<PathBuf>, root: bool) -> bool {
    let Ok(canonical) = fs::canonicalize(path) else {
        return false;
    };
    if target.starts_with(&canonical) {
        return true;
    }
    if !canonical.is_dir() || !ancestry.insert(canonical.clone()) {
        return false;
    }
    if !root
        && (path.join("SKILL.md").exists()
            || path
                .join(format!(
                    "{}.md",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ))
                .exists())
    {
        ancestry.remove(&canonical);
        return false;
    }
    let allowed = fs::read_dir(path).ok().is_some_and(|entries| {
        entries.flatten().any(|entry| {
            !entry.file_name().to_string_lossy().starts_with('.')
                && user_allows(&entry.path(), target, ancestry, false)
        })
    });
    ancestry.remove(&canonical);
    allowed
}
pub fn read_skill_file_checked(
    path: &Path,
    project: &Path,
    user_roots: &[PathBuf],
) -> Result<String, SkillReadError> {
    let canonical = check_skill_path(path, project, user_roots)?;
    fs::read_to_string(canonical).map_err(|error| SkillReadError::Io {
        path: path.to_path_buf(),
        error,
    })
}
pub fn read_project_skills(
    dir: &Path,
    project: &Path,
    user_roots: &[PathBuf],
) -> CheckedProjectSkills {
    let mut result = CheckedProjectSkills::default();
    if dir.exists() {
        walk(
            dir,
            project,
            user_roots,
            &HashSet::new(),
            true,
            false,
            &mut result,
        );
    }
    result
}
fn walk(
    path: &Path,
    project: &Path,
    user_roots: &[PathBuf],
    ancestry: &HashSet<PathBuf>,
    root: bool,
    markdown: bool,
    out: &mut CheckedProjectSkills,
) {
    let result = (|| -> Result<(), SkillReadError> {
        let canonical = check_skill_path(path, project, user_roots)?;
        let metadata = fs::metadata(&canonical).map_err(|error| SkillReadError::Io {
            path: path.to_path_buf(),
            error,
        })?;
        if metadata.is_dir() {
            if ancestry.contains(&canonical) {
                return Ok(());
            }
            let mut next = ancestry.clone();
            next.insert(canonical);
            let entries = fs::read_dir(path).map_err(|error| SkillReadError::Io {
                path: path.to_path_buf(),
                error,
            })?;
            let mut entries =
                entries
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|error| SkillReadError::Io {
                        path: path.to_path_buf(),
                        error,
                    })?;
            entries.sort_by_key(fs::DirEntry::file_name);
            if entries
                .iter()
                .any(|e| e.file_name() == "SKILL.md" && !e.file_type().is_ok_and(|t| t.is_dir()))
            {
                walk(
                    &path.join("SKILL.md"),
                    project,
                    user_roots,
                    &next,
                    false,
                    false,
                    out,
                );
                return Ok(());
            }
            for entry in entries {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with('.') || name == "node_modules" {
                    continue;
                }
                let kind = entry.file_type().map_err(|error| SkillReadError::Io {
                    path: entry.path(),
                    error,
                })?;
                if kind.is_dir() || kind.is_symlink() || (root && name.ends_with(".md")) {
                    walk(&entry.path(), project, user_roots, &next, false, root, out);
                }
            }
        } else if path.file_name().is_some_and(|name| name == "SKILL.md")
            || (markdown && path.extension().is_some_and(|ext| ext == "md"))
        {
            let raw = read_skill_file_checked(path, project, user_roots)?;
            let fallback = if path.file_name().is_some_and(|name| name == "SKILL.md") {
                path.parent().and_then(Path::file_name)
            } else {
                path.file_stem()
            };
            let name = frontmatter_name(&raw)
                .unwrap_or_else(|| fallback.unwrap_or_default().to_string_lossy().into_owned());
            out.skills
                .entry(name.clone())
                .or_insert_with(|| LoadedSkill {
                    name,
                    content: skill_body(&raw).trim().to_string(),
                    location: Some(path.to_string_lossy().into_owned()),
                });
        }
        Ok(())
    })();
    if let Err(error) = result {
        match error {
            SkillReadError::Blocked { .. } => out.blocked.push(error),
            SkillReadError::Io { .. } => out.errors.push(error),
        }
    }
}
