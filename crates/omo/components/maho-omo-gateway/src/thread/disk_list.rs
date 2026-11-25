//! Bounded disk session listing (`thread/disk-list.ts`).

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use super::address_book::{DiskSession, read_disk_session, session_directory_prefix};

/// How many session files one `thread_list` reads for its inactive rows: the newest by file time.
pub const DISK_LIST_MAX_SESSIONS: usize = 200;

/// How many session files one `thread_list` stats to rank by modification time: the newest by file name.
pub const DISK_LIST_MAX_STATS: usize = 1000;

#[derive(Debug, Clone, Default)]
pub struct ListDiskSessionsOptions {
    pub all_scope: bool,
    pub workspace_roots: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct DiskListResult {
    pub sessions: Vec<DiskSession>,
    pub unread: Vec<String>,
}

fn file_durable_id(path: &Path) -> String {
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
    stem.rsplit('_').next().unwrap_or("").to_owned()
}

fn session_paths_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect()
}

fn is_project_dir(entry: &std::fs::DirEntry) -> bool {
    let Ok(ft) = entry.file_type() else {
        return false;
    };
    if !ft.is_dir() && !ft.is_symlink() {
        return false;
    }
    let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
        return false;
    };
    name.starts_with("--") && name.ends_with("--") && name.len() >= 4
}

pub fn list_disk_sessions(sessions_dir: &Path, opts: &ListDiskSessionsOptions) -> DiskListResult {
    let Ok(rd) = std::fs::read_dir(sessions_dir) else {
        return DiskListResult::default();
    };
    let project_dirs: Vec<String> = rd
        .flatten()
        .filter(is_project_dir)
        .filter_map(|e| e.file_name().to_str().map(str::to_owned))
        .collect();

    let prefixes: Option<Vec<String>> = (!opts.all_scope).then(|| {
        opts.workspace_roots
            .iter()
            .map(|r| session_directory_prefix(r))
            .collect()
    });

    let mut paths: Vec<PathBuf> = project_dirs
        .into_iter()
        .filter(|dir| match &prefixes {
            None => true,
            Some(prefs) => prefs.iter().any(|p| dir.starts_with(p)),
        })
        .flat_map(|dir| session_paths_in(&sessions_dir.join(dir)))
        .collect();

    paths.sort_by(|left, right| {
        let lb = left.file_name().and_then(|s| s.to_str()).unwrap_or("");
        let rb = right.file_name().and_then(|s| s.to_str()).unwrap_or("");
        rb.cmp(lb).then_with(|| left.cmp(right))
    });

    let mut files: Vec<(PathBuf, u128)> = paths
        .iter()
        .take(DISK_LIST_MAX_STATS)
        .filter_map(|p| {
            let meta = std::fs::metadata(p).ok()?;
            if !meta.is_file() {
                return None;
            }
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            Some((p.clone(), modified))
        })
        .collect();

    files.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));

    let read_files: Vec<PathBuf> = files
        .into_iter()
        .take(DISK_LIST_MAX_SESSIONS)
        .map(|(p, _)| p)
        .collect();

    let sessions: Vec<DiskSession> = read_files
        .iter()
        .filter_map(|p| read_disk_session(p, None))
        .collect();

    let read_set: std::collections::HashSet<PathBuf> = read_files.into_iter().collect();
    let unread: Vec<String> = paths
        .into_iter()
        .filter(|p| !read_set.contains(p))
        .map(|p| file_durable_id(&p))
        .collect();

    DiskListResult { sessions, unread }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_disk_sessions_reads_all_when_few_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        let proj = dir.path().join("--repo--");
        std::fs::create_dir_all(&proj).unwrap();
        let older = proj.join("2026-01-01T00-00-00_id-old.jsonl");
        let newer = proj.join("2026-01-02T00-00-00_id-new.jsonl");
        std::fs::write(
            &older,
            r#"{"type":"session","id":"id-old","cwd":"/repo","timestamp":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(15));
        std::fs::write(
            &newer,
            r#"{"type":"session","id":"id-new","cwd":"/repo","timestamp":"2026-01-02T00:00:00Z"}"#,
        )
        .unwrap();

        let listed = list_disk_sessions(
            dir.path(),
            &ListDiskSessionsOptions {
                all_scope: false,
                workspace_roots: vec!["/repo".into()],
            },
        );
        assert!(listed.unread.is_empty());
        assert_eq!(listed.sessions.len(), 2);
        assert_eq!(listed.sessions[0].durable_id, "id-new");
        assert_eq!(listed.sessions[1].durable_id, "id-old");
    }

    #[test]
    fn list_disk_sessions_filters_outside_workspace_unless_all_scope() {
        let dir = tempfile::tempdir().unwrap();
        let proj_a = dir.path().join("--repo-a--");
        let proj_b = dir.path().join("--repo-b--");
        std::fs::create_dir_all(&proj_a).unwrap();
        std::fs::create_dir_all(&proj_b).unwrap();
        std::fs::write(
            proj_a.join("2026-01-01_id-a.jsonl"),
            r#"{"type":"session","id":"id-a","cwd":"/repo/a","timestamp":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();
        std::fs::write(
            proj_b.join("2026-01-01_id-b.jsonl"),
            r#"{"type":"session","id":"id-b","cwd":"/repo/b","timestamp":"2026-01-01T00:00:00Z"}"#,
        )
        .unwrap();

        let scoped = list_disk_sessions(
            dir.path(),
            &ListDiskSessionsOptions {
                all_scope: false,
                workspace_roots: vec!["/repo/a".into()],
            },
        );
        assert_eq!(scoped.sessions.len(), 1);
        assert_eq!(scoped.sessions[0].durable_id, "id-a");

        let all = list_disk_sessions(
            dir.path(),
            &ListDiskSessionsOptions {
                all_scope: true,
                workspace_roots: vec!["/repo/a".into()],
            },
        );
        assert_eq!(all.sessions.len(), 2);
        assert!(all.unread.is_empty());
    }
}
