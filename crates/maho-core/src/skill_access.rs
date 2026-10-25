//! Project skill path authorization, canonical-root allowlists, symlink containment,
//! and descriptor-checked reads.
//!
//! Port of senpi `packages/coding-agent/src/core/skill-access.ts`.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use sha2::{Digest, Sha256};

use crate::diagnostics::ResourceDiagnostic;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkillProvenanceKind {
    User,
    ProjectAuto,
    ProjectSelected,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillProvenance {
    pub kind: SkillProvenanceKind,
    pub selected_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_root: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SkillAllowedSet {
    pub roots: Vec<String>,
    pub files: Vec<String>,
    pub digest: String,
}

pub fn skill_project_root(cwd: &Path) -> PathBuf {
    let resolved = std::fs::canonicalize(cwd).unwrap_or_else(|_| cwd.to_path_buf());
    let mut current = resolved.as_path();
    loop {
        if current.join(".git").exists() {
            return current.to_path_buf();
        }
        match current.parent() {
            Some(parent) if parent != current => current = parent,
            _ => return resolved,
        }
    }
}

pub fn inside_skill_root(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

pub fn build_skill_allowed_set(sources: &[PathBuf]) -> SkillAllowedSet {
    let mut roots = BTreeSet::new();
    let mut files = BTreeSet::new();

    for source in sources {
        let canonical = match std::fs::canonicalize(source) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if canonical.is_file() {
            files.insert(canonical.to_string_lossy().into_owned());
        } else if canonical.is_dir() {
            roots.insert(canonical.to_string_lossy().into_owned());
        }
    }

    let allowed_roots: Vec<String> = roots.into_iter().collect();
    let allowed_files: Vec<String> = files.into_iter().collect();

    let mut hasher = Sha256::new();
    let serialized = serde_json::to_string(&(&allowed_roots, &allowed_files)).unwrap_or_default();
    hasher.update(serialized.as_bytes());
    let digest = format!("{:x}", hasher.finalize());

    SkillAllowedSet {
        roots: allowed_roots,
        files: allowed_files,
        digest,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockedSkillLinkError {
    pub message: String,
    pub diagnostic: Box<ResourceDiagnostic>,
}

impl BlockedSkillLinkError {
    pub fn new(path: &Path, target: &Path) -> Self {
        let display = |p: &Path| -> String {
            let home = crate::config::home_dir_path();
            if inside_skill_root(p, &home)
                && let Ok(stripped) = p.strip_prefix(&home) {
                    return format!("~{}{}", std::path::MAIN_SEPARATOR, stripped.display());
                }
            p.display().to_string()
        };
        let msg = format!(
            "Blocked skill link outside the project: {} -> {}. Add the target folder to your user skills settings to allow it.",
            display(path),
            display(target)
        );
        let diag = ResourceDiagnostic {
            category: Some("skill-link-blocked".into()),
            diagnostic_type: crate::diagnostics::ResourceDiagnosticType::Warning,
            message: msg.clone(),
            path: Some(path.to_string_lossy().into_owned()),
            collision: None,
        };
        Self {
            message: msg,
            diagnostic: Box::new(diag),
        }
    }
}

impl std::fmt::Display for BlockedSkillLinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for BlockedSkillLinkError {}

pub fn authorize_skill_path(
    path: &Path,
    provenance: Option<&SkillProvenance>,
    allowed: &SkillAllowedSet,
    directory: bool,
) -> Result<PathBuf, BlockedSkillLinkError> {
    if provenance.is_none() || matches!(provenance.map(|p| p.kind), Some(SkillProvenanceKind::User)) {
        return Ok(path.to_path_buf());
    }

    let Some(prov) = provenance else { return Ok(path.to_path_buf()); };
    let canonical = match std::fs::canonicalize(path) {
        Ok(c) => c,
        Err(_) => return Err(BlockedSkillLinkError::new(path, Path::new("unresolvable target"))),
    };

    let mut allowed_roots = Vec::new();
    if let Some(ref pr) = prov.project_root
        && let Ok(c) = std::fs::canonicalize(pr) {
            allowed_roots.push(c);
        }
    if prov.kind == SkillProvenanceKind::ProjectSelected
        && let Ok(c) = std::fs::canonicalize(&prov.selected_root) {
            allowed_roots.push(c);
        }
    for root in &allowed.roots {
        if let Ok(c) = std::fs::canonicalize(root) {
            allowed_roots.push(c);
        }
    }

    if allowed_roots.iter().any(|root| inside_skill_root(&canonical, root)) {
        return Ok(canonical);
    }

    let canonical_str = canonical.to_string_lossy();
    if allowed.files.iter().any(|f| f == canonical_str.as_ref()) {
        return Ok(canonical);
    }

    if directory && allowed.files.iter().any(|f| inside_skill_root(Path::new(f), &canonical)) {
        return Ok(canonical);
    }

    Err(BlockedSkillLinkError::new(path, &canonical))
}

pub fn read_skill_file_checked(
    path: &Path,
    provenance: Option<&SkillProvenance>,
    allowed: &SkillAllowedSet,
) -> Result<String, BlockedSkillLinkError> {
    if provenance.is_none() || matches!(provenance.map(|p| p.kind), Some(SkillProvenanceKind::User)) {
        return std::fs::read_to_string(path).map_err(|_| BlockedSkillLinkError::new(path, path));
    }

    let canonical = authorize_skill_path(path, provenance, allowed, false)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let expected = std::fs::symlink_metadata(&canonical)
            .map_err(|_| BlockedSkillLinkError::new(path, &canonical))?;
        if !expected.is_file() {
            return Err(BlockedSkillLinkError::new(path, &canonical));
        }

        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new().read(true).custom_flags(nix::libc::O_NOFOLLOW).open(&canonical)
            .map_err(|_| BlockedSkillLinkError::new(path, &canonical))?;
        let opened = file
            .metadata()
            .map_err(|_| BlockedSkillLinkError::new(path, &canonical))?;

        if !opened.is_file()
            || opened.dev() != expected.dev()
            || opened.ino() != expected.ino()
        {
            return Err(BlockedSkillLinkError::new(path, &canonical));
        }

        use std::io::Read;
        let mut reader = std::io::BufReader::new(file);
        let mut content = String::new();
        reader
            .read_to_string(&mut content)
            .map_err(|_| BlockedSkillLinkError::new(path, &canonical))?;
        Ok(content)
    }

    #[cfg(not(unix))]
    {
        let expected = std::fs::symlink_metadata(&canonical)
            .map_err(|_| BlockedSkillLinkError::new(path, &canonical))?;
        if !expected.is_file() {
            return Err(BlockedSkillLinkError::new(path, &canonical));
        }
        std::fs::read_to_string(&canonical).map_err(|_| BlockedSkillLinkError::new(path, &canonical))
    }
}

pub fn select_skill_root(
    root: &str,
    scope: crate::source_info::SourceScope,
    cwd: &str,
    selected: bool,
) -> SkillProvenance {
    let spelled = PathBuf::from(crate::paths::lexical_resolve(root));
    let repository = skill_project_root(Path::new(cwd));
    let owner = spelled.parent().and_then(Path::parent);
    let ancestor = spelled.file_name().is_some_and(|name| name == "skills")
        && spelled
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| [".agents", ".senpi", ".pi"].iter().any(|s| name == *s))
        && owner.is_some_and(|owner| Path::new(cwd).starts_with(owner));
    let project_root = if !selected && ancestor && !spelled.starts_with(&repository) {
        owner.unwrap_or(&repository).to_string_lossy().into()
    } else {
        repository.to_string_lossy().into()
    };
    SkillProvenance {
        kind: if scope == crate::source_info::SourceScope::Project {
            if selected {
                SkillProvenanceKind::ProjectSelected
            } else {
                SkillProvenanceKind::ProjectAuto
            }
        } else {
            SkillProvenanceKind::User
        },
        selected_root: root.into(),
        project_root: Some(project_root),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_provenance_permits_any_path() {
        let path = Path::new("/any/path/SKILL.md");
        let prov = SkillProvenance {
            kind: SkillProvenanceKind::User,
            selected_root: "/any".into(),
            project_root: None,
        };
        let allowed = SkillAllowedSet::default();
        assert_eq!(
            authorize_skill_path(path, Some(&prov), &allowed, false).unwrap(),
            path
        );
    }

    #[test]
    fn inside_root_checks() {
        assert!(inside_skill_root(Path::new("/a/b/c"), Path::new("/a/b")));
        assert!(inside_skill_root(Path::new("/a/b"), Path::new("/a/b")));
        assert!(!inside_skill_root(Path::new("/a/other"), Path::new("/a/b")));
    }
}
