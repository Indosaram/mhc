//! Merge raw project evidence into candidates (upstream `project-candidates.ts`).

use std::{collections::HashMap, path::Path};

use super::git_identity::{GitIdentity, read_git_identity};
use super::project_paths::{ProjectPathRules, normalize_project_path_for_comparison};
use super::session_history_fs::{FileKind, SessionHistoryFs, real_path_or, stat_or_null};
use super::types::{ImportedProject, ProjectCandidate, ProjectGit, ProjectSource};

#[derive(Clone, Debug)]
pub struct RawProjectEvidence {
    pub cwd: String,
    pub source: ProjectSource,
    pub thread_count: usize,
    pub last_active_at_ms: Option<i64>,
}

struct MergedEntry {
    path: String,
    sources: Vec<ProjectSource>,
    thread_count: usize,
    last_active_at_ms: Option<i64>,
    git: Option<ProjectGit>,
}

/// `None` means excluded.
fn resolve_directory_key(
    fs: &dyn SessionHistoryFs,
    rules: &ProjectPathRules,
    resolved: &str,
    git_identities: &mut HashMap<String, Option<ProjectGit>>,
) -> Option<String> {
    let stats = stat_or_null(fs, resolved)?;
    if stats.kind != FileKind::Directory {
        return None;
    }
    if rules.is_excluded_project_path(&real_path_or(fs, resolved)) {
        return None;
    }
    let git = match read_git_identity(fs, resolved) {
        GitIdentity::Worktree => return None,
        GitIdentity::Repository(git) => Some(git),
        GitIdentity::NotGit => None,
    };
    let key = rules.directory_identity(resolved, Some(&stats));
    git_identities.insert(key.clone(), git);
    Some(key)
}

fn basename(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Merge per-source evidence by filesystem identity, keeping the first spelling and any persisted project path.
pub fn build_project_candidates(
    fs: &dyn SessionHistoryFs,
    rules: &ProjectPathRules,
    raw: &[RawProjectEvidence],
    imported_projects: &[ImportedProject],
) -> Vec<ProjectCandidate> {
    let mut order: Vec<String> = Vec::new();
    let mut merged: HashMap<String, MergedEntry> = HashMap::new();
    let mut directory_keys: HashMap<String, Option<String>> = HashMap::new();
    let mut git_identities: HashMap<String, Option<ProjectGit>> = HashMap::new();
    for evidence in raw {
        let Some(resolved) = rules.resolve_recorded_path(&evidence.cwd) else {
            continue;
        };
        if rules.is_excluded_project_path(&resolved) {
            continue;
        }
        let key = match directory_keys.get(&resolved) {
            Some(key) => key.clone(),
            None => {
                let key = resolve_directory_key(fs, rules, &resolved, &mut git_identities);
                directory_keys.insert(resolved.clone(), key.clone());
                key
            }
        };
        let Some(key) = key else { continue };
        match merged.get_mut(&key) {
            None => {
                order.push(key.clone());
                merged.insert(
                    key.clone(),
                    MergedEntry {
                        path: resolved,
                        sources: vec![evidence.source],
                        thread_count: evidence.thread_count,
                        last_active_at_ms: evidence.last_active_at_ms,
                        git: git_identities.get(&key).cloned().flatten(),
                    },
                );
            }
            Some(existing) => {
                if !existing.sources.contains(&evidence.source) {
                    existing.sources.push(evidence.source);
                }
                existing.thread_count += evidence.thread_count;
                existing.last_active_at_ms =
                    match (existing.last_active_at_ms, evidence.last_active_at_ms) {
                        (Some(a), Some(b)) => Some(a.max(b)),
                        (a, b) => a.or(b),
                    };
            }
        }
    }

    let mut imported_by_root: HashMap<String, &ImportedProject> = HashMap::new();
    for project in imported_projects {
        let root = rules
            .resolve_recorded_path(&project.workspace_root)
            .unwrap_or_else(|| project.workspace_root.clone());
        imported_by_root.insert(normalize_project_path_for_comparison(&root), project);
        imported_by_root.insert(rules.directory_identity(&root, None), project);
    }

    let mut candidates = Vec::new();
    for key in order {
        let Some(entry) = merged.remove(&key) else {
            continue;
        };
        let imported = imported_by_root
            .get(&normalize_project_path_for_comparison(&entry.path))
            .or_else(|| imported_by_root.get(&key))
            .copied();
        let candidate_path =
            imported.map_or_else(|| entry.path.clone(), |p| p.workspace_root.clone());
        let title = Some(basename(&candidate_path))
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| candidate_path.clone());
        candidates.push(ProjectCandidate {
            path: candidate_path,
            title,
            project_id: imported.map(|p| p.id.clone()),
            sources: entry.sources,
            thread_count: entry.thread_count,
            last_active_at: entry
                .last_active_at_ms
                .map(super::transcript_parser::iso_from_ms),
            already_imported: imported.is_some(),
            git: entry.git,
        });
    }
    candidates.sort_by(
        |left, right| match (&left.last_active_at, &right.last_active_at) {
            (a, b) if a == b => left.path.cmp(&right.path),
            (None, _) => std::cmp::Ordering::Greater,
            (_, None) => std::cmp::Ordering::Less,
            (Some(a), Some(b)) => b.cmp(a),
        },
    );
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_history::project_paths::{ProjectPathRulesInput, create_project_path_rules};
    use crate::session_history::session_history_fs::node_session_history_fs;
    use crate::session_history::types::AgentSessionSource;

    #[test]
    fn merges_by_directory_and_marks_imported_and_drops_worktrees()
    -> Result<(), Box<dyn std::error::Error>> {
        let dir = tempfile::tempdir()?;
        let project = dir.path().join("proj");
        let worktree = dir.path().join("wt");
        std::fs::create_dir_all(&project)?;
        std::fs::create_dir_all(&worktree)?;
        std::fs::write(worktree.join(".git"), "gitdir: /r/.git/worktrees/wt\n")?;
        let fs = node_session_history_fs();
        let rules = create_project_path_rules(ProjectPathRulesInput {
            home_dir: Some("/nonexistent-home".into()),
            tmp_dir: Some("/nonexistent-tmp".into()),
            excluded_directories: Some(vec![]),
            managed_worktree_directories: None,
            platform: Some("linux".into()),
            fs: fs.clone(),
        });
        let p = project.to_string_lossy().into_owned();
        let raw = vec![
            RawProjectEvidence {
                cwd: p.clone(),
                source: ProjectSource::Agent(AgentSessionSource::Codex),
                thread_count: 2,
                last_active_at_ms: Some(1000),
            },
            RawProjectEvidence {
                cwd: format!("{p}/"),
                source: ProjectSource::Omo,
                thread_count: 0,
                last_active_at_ms: Some(5000),
            },
            RawProjectEvidence {
                cwd: worktree.to_string_lossy().into_owned(),
                source: ProjectSource::Omo,
                thread_count: 0,
                last_active_at_ms: None,
            },
            RawProjectEvidence {
                cwd: dir.path().join("missing").to_string_lossy().into_owned(),
                source: ProjectSource::Omo,
                thread_count: 0,
                last_active_at_ms: None,
            },
        ];
        let imported = vec![ImportedProject {
            id: "id1".into(),
            workspace_root: p.clone(),
        }];
        let candidates = build_project_candidates(fs.as_ref(), &rules, &raw, &imported);
        assert_eq!(candidates.len(), 1);
        let only = &candidates[0];
        assert_eq!(only.thread_count, 2);
        assert_eq!(only.sources.len(), 2);
        assert!(only.already_imported);
        assert_eq!(only.project_id.as_deref(), Some("id1"));
        assert_eq!(only.title, "proj");
        assert_eq!(
            only.last_active_at.as_deref(),
            Some("1970-01-01T00:00:05.000Z")
        );
        Ok(())
    }
}
