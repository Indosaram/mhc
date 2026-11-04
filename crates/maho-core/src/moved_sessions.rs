//! Port of senpi `core/moved-sessions.ts` (senpi#2184, senpi#2990).
//!
//! A session is "moved" when it belongs to the current repository (by its recorded identity) and was
//! recorded at a path that no longer exists: the repository moved away from it. A live second checkout
//! is not moved, and an unrecorded or different repository never matches.
use std::future::Future;
use std::io::BufRead;
use std::path::Path;

use crate::moved_session_cwd::session_cwd_matcher_with;
use crate::paths::{PathInputOptions, normalize_path, resolve_path};
use crate::repository_identity::{compare_repository_identities, read_repository_identity};
use crate::session_discovery::{SessionInfo, list_sessions_from_dir};
use crate::session_move::bundle::RepositoryIdentity;

/// A session recorded at a vanished path is "here" when the OmO desktop moved that path to `cwd`: it is
/// this folder's own session, listed without the moved badge and never rebound (senpi#2990). A trusted
/// breadcrumb decides that, so it needs no git identity; only a "moved" session (senpi#2184) is matched
/// by the folder's identity.
#[derive(Clone, Copy, PartialEq, Eq)]
enum VanishedKind {
    Here,
    Moved,
}

fn resolved(path: &str) -> String {
    resolve_path(path, path, &PathInputOptions::default())
}

fn exists(path: &str) -> bool {
    Path::new(path).exists()
}

/// Where sessions live and how a recorded cwd maps through a desktop move; injected by tests.
struct Layout<'a> {
    sessions_root: String,
    own_dir: String,
    resolve_moved: &'a dyn Fn(&str) -> String,
}

impl Layout<'static> {
    fn real(cwd: &str) -> Self {
        Self {
            sessions_root: crate::config::get_sessions_dir(),
            own_dir: crate::session_manager::get_default_session_dir_path(
                cwd,
                &crate::config::get_agent_dir(),
            ),
            resolve_moved: &crate::session_manager::resolve_moved_session_path,
        }
    }
}

struct VanishedClassifier<M> {
    cwd: String,
    current: Option<RepositoryIdentity>,
    own_cwd: M,
}

impl<M: FnMut(Option<&str>) -> bool> VanishedClassifier<M> {
    fn kind(&mut self, session: &SessionInfo) -> Option<VanishedKind> {
        if session.cwd.is_empty() || resolved(&session.cwd) == self.cwd || exists(&session.cwd) {
            return None;
        }
        if (self.own_cwd)(Some(&session.cwd)) {
            return Some(VanishedKind::Here);
        }
        let current = self.current.as_ref()?;
        (compare_repository_identities(session.repository_identity.as_ref(), Some(current))
            == "same")
            .then_some(VanishedKind::Moved)
    }
}

fn vanished_classifier(
    cwd: &str,
    current: Option<RepositoryIdentity>,
    layout: &Layout<'_>,
) -> VanishedClassifier<impl FnMut(Option<&str>) -> bool> {
    VanishedClassifier {
        cwd: cwd.to_owned(),
        current,
        own_cwd: session_cwd_matcher_with(cwd, layout.resolve_moved),
    }
}

fn newest_first(a: &SessionInfo, b: &SessionInfo) -> std::cmp::Ordering {
    b.modified.cmp(&a.modified)
}

/// A session dir that cannot be read contributes nothing; it never fails a listing or --continue
/// (senpi#2990 review L4).
fn readable_entries(dir: &str) -> Vec<std::fs::DirEntry> {
    std::fs::read_dir(dir)
        .map(|entries| entries.filter_map(Result::ok).collect())
        .unwrap_or_default()
}

fn recorded_cwd(dir: &str) -> Option<String> {
    for entry in readable_entries(dir) {
        let name = entry.file_name();
        if !name.to_string_lossy().ends_with(".jsonl") {
            continue;
        }
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        let mut first_line = String::new();
        if std::io::BufReader::new(file)
            .read_line(&mut first_line)
            .is_err()
        {
            continue;
        }
        let Ok(header) = serde_json::from_str::<serde_json::Value>(&first_line) else {
            continue;
        };
        if let Some(cwd) = header.get("cwd").and_then(serde_json::Value::as_str) {
            return Some(cwd.to_owned());
        }
    }
    None
}

/// Each project directory holds one cwd, so one header decides whether its sessions can be moved ones;
/// only directories whose recorded path is gone and that `keep` accepts are listed in full.
fn sessions_at_vanished_paths(
    layout: &Layout<'_>,
    mut keep: impl FnMut(&str) -> bool,
) -> Vec<SessionInfo> {
    if !exists(&layout.sessions_root) {
        return Vec::new();
    }
    let own = normalize_path(&layout.own_dir, &PathInputOptions::default());
    let mut sessions = Vec::new();
    for entry in readable_entries(&layout.sessions_root) {
        if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let dir = entry.path().to_string_lossy().into_owned();
        if normalize_path(&dir, &PathInputOptions::default()) == own {
            continue;
        }
        let Some(recorded) = recorded_cwd(&dir) else {
            continue;
        };
        if exists(&recorded) || !keep(&recorded) {
            continue;
        }
        sessions.extend(list_sessions_from_dir(&dir, None, 0, None));
    }
    sessions
}

#[derive(Default)]
struct Vanished {
    here: Vec<SessionInfo>,
    moved: Vec<SessionInfo>,
}

/// Sessions at vanished paths of the `wanted` kinds, newest first; the identity is read only when
/// "moved" is wanted.
async fn vanished_sessions<F, Fut>(
    cwd: &str,
    session_dir: Option<&str>,
    read_identity: F,
    wanted: &[VanishedKind],
    layout: &Layout<'_>,
) -> Vanished
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<RepositoryIdentity>>,
{
    let mut found = Vanished::default();
    let here = resolved(cwd);
    let want_here = wanted.contains(&VanishedKind::Here);
    let current = if wanted.contains(&VanishedKind::Moved) {
        read_identity(here.clone()).await
    } else {
        None
    };
    if !want_here && current.is_none() {
        return found;
    }
    let has_current = current.is_some();
    let mut classifier = vanished_classifier(&here, current, layout);
    let candidates = match session_dir {
        Some(dir) => list_sessions_from_dir(
            &normalize_path(dir, &PathInputOptions::default()),
            None,
            0,
            None,
        ),
        None => sessions_at_vanished_paths(layout, |recorded| {
            has_current || (classifier.own_cwd)(Some(recorded))
        }),
    };
    for session in candidates {
        match classifier.kind(&session) {
            Some(VanishedKind::Moved) => found.moved.push(SessionInfo {
                moved: true,
                ..session
            }),
            Some(VanishedKind::Here) if want_here => found.here.push(session),
            _ => {}
        }
    }
    found.here.sort_by(newest_first);
    found.moved.sort_by(newest_first);
    found
}

/// senpi `listMovedSessions`, reading the current repository identity with `git`.
pub async fn list_moved_sessions(cwd: &str, session_dir: Option<&str>) -> Vec<SessionInfo> {
    list_moved_sessions_with(cwd, session_dir, |dir| async move {
        read_repository_identity(&dir).await
    })
    .await
}

/// senpi `listMovedSessions` with an injected identity reader (`MovedSessionOptions.readIdentity`).
pub async fn list_moved_sessions_with<F, Fut>(
    cwd: &str,
    session_dir: Option<&str>,
    read_identity: F,
) -> Vec<SessionInfo>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<RepositoryIdentity>>,
{
    let layout = Layout::real(cwd);
    vanished_sessions(
        cwd,
        session_dir,
        read_identity,
        &[VanishedKind::Moved],
        &layout,
    )
    .await
    .moved
}

/// findMostRecentSession's basis; a file that vanished since it was listed counts as oldest.
fn mtime_ms(path: &str) -> u128 {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |duration| duration.as_millis())
}

/// `--continue` in the default per-folder layout (senpi#2990): the newest session the OmO desktop moved
/// to `cwd` when it is newer than the folder's own newest, else None. It lists only the dirs whose
/// recorded path moved here and reads no git identity.
pub async fn moved_here_session_to_continue(cwd: &str) -> Option<String> {
    let layout = Layout::real(cwd);
    let own_dir = crate::session_manager::get_default_session_dir(cwd);
    moved_here_session_to_continue_in(cwd, &own_dir, &layout).await
}

async fn moved_here_session_to_continue_in(
    cwd: &str,
    own_dir: &str,
    layout: &Layout<'_>,
) -> Option<String> {
    let no_identity = |_: String| async { None };
    let here = vanished_sessions(cwd, None, no_identity, &[VanishedKind::Here], layout)
        .await
        .here;
    let newest = here.into_iter().map(|session| session.path).fold(
        None::<String>,
        |best, path| match best {
            Some(best) if mtime_ms(&path) <= mtime_ms(&best) => Some(best),
            _ => Some(path),
        },
    )?;
    match crate::session_manager::find_most_recent_session(own_dir, None) {
        Some(own) if mtime_ms(&newest) <= mtime_ms(&own) => None,
        _ => Some(newest),
    }
}

/// senpi `markMovedSessions`, reading the current repository identity with `git`.
pub async fn mark_moved_sessions(sessions: &[SessionInfo], cwd: &str) -> Vec<SessionInfo> {
    mark_moved_sessions_with(sessions, cwd, |dir| async move {
        read_repository_identity(&dir).await
    })
    .await
}

/// senpi `markMovedSessions` with an injected identity reader.
pub async fn mark_moved_sessions_with<F, Fut>(
    sessions: &[SessionInfo],
    cwd: &str,
    read_identity: F,
) -> Vec<SessionInfo>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<RepositoryIdentity>>,
{
    let layout = Layout::real(cwd);
    mark_moved_sessions_in(sessions, cwd, read_identity, &layout).await
}

async fn mark_moved_sessions_in<F, Fut>(
    sessions: &[SessionInfo],
    cwd: &str,
    read_identity: F,
    layout: &Layout<'_>,
) -> Vec<SessionInfo>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<RepositoryIdentity>>,
{
    let here = resolved(cwd);
    let Some(current) = read_identity(here.clone()).await else {
        return sessions.to_vec();
    };
    let mut classifier = vanished_classifier(&here, Some(current), layout);
    sessions
        .iter()
        .map(|session| {
            let moved = classifier.kind(session) == Some(VanishedKind::Moved);
            SessionInfo {
                moved: moved || session.moved,
                ..session.clone()
            }
        })
        .collect()
}

/// senpi `withMovedSessions`, reading the current repository identity with `git`. `local` is the
/// folder's own listing (senpi awaits it alongside the vanished lookup).
pub async fn with_moved_sessions(
    local: Vec<SessionInfo>,
    cwd: &str,
    session_dir: Option<&str>,
) -> Vec<SessionInfo> {
    with_moved_sessions_with(local, cwd, session_dir, |dir| async move {
        read_repository_identity(&dir).await
    })
    .await
}

/// senpi `withMovedSessions` with an injected identity reader.
pub async fn with_moved_sessions_with<F, Fut>(
    local: Vec<SessionInfo>,
    cwd: &str,
    session_dir: Option<&str>,
    read_identity: F,
) -> Vec<SessionInfo>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<RepositoryIdentity>>,
{
    let layout = Layout::real(cwd);
    with_moved_sessions_in(local, cwd, session_dir, read_identity, &layout).await
}

async fn with_moved_sessions_in<F, Fut>(
    local: Vec<SessionInfo>,
    cwd: &str,
    session_dir: Option<&str>,
    read_identity: F,
    layout: &Layout<'_>,
) -> Vec<SessionInfo>
where
    F: Fn(String) -> Fut,
    Fut: Future<Output = Option<RepositoryIdentity>>,
{
    // A shared session dir's own listing already holds the sessions the desktop moved here, so only the
    // default per-folder layout looks for them elsewhere.
    let wanted: &[VanishedKind] = if session_dir.is_some() {
        &[VanishedKind::Moved]
    } else {
        &[VanishedKind::Here, VanishedKind::Moved]
    };
    let vanished = vanished_sessions(cwd, session_dir, read_identity, wanted, layout).await;
    // Defensive dedupe: a session listed by both lookups stays one row, the folder's own.
    let listed: std::collections::HashSet<String> =
        local.iter().map(|session| session.path.clone()).collect();
    let mut all = local;
    all.extend(
        vanished
            .here
            .into_iter()
            .chain(vanished.moved)
            .filter(|session| !listed.contains(&session.path)),
    );
    all.sort_by(newest_first);
    all
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ROOT_COMMIT: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const OTHER_COMMIT: &str = "dddddddddddddddddddddddddddddddddddddddd";

    fn recorded() -> RepositoryIdentity {
        RepositoryIdentity {
            root_commits: vec![ROOT_COMMIT.to_owned()],
            origin_url: None,
        }
    }

    fn write_session(
        dir: &Path,
        cwd: &Path,
        id: &str,
        identity: Option<&str>,
        text: &str,
    ) -> String {
        std::fs::create_dir_all(dir).expect("session dir");
        let file = dir.join(format!("2026-09-27_{id}.jsonl"));
        let mut lines = vec![
            json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-09-27T00:00:00.000Z", "cwd": cwd}).to_string(),
            json!({"type": "message", "id": "u1", "parentId": null, "timestamp": "2026-09-27T00:00:00.000Z", "message": {"role": "user", "content": text, "timestamp": 1}}).to_string(),
        ];
        if let Some(root) = identity {
            lines.push(json!({"type": "custom", "customType": "repository-identity", "data": {"rootCommits": [root]}}).to_string());
        }
        std::fs::write(&file, lines.join("\n")).expect("session file");
        file.to_string_lossy().into_owned()
    }

    struct Fixture {
        _root: tempfile::TempDir,
        cwd: String,
        old_cwd: String,
        sessions_root: std::path::PathBuf,
        moved: String,
        files: Vec<String>,
    }

    fn dir_for(sessions_root: &Path, shared: Option<&Path>, cwd: &Path) -> std::path::PathBuf {
        shared.map_or_else(
            || sessions_root.join(cwd.to_string_lossy().replace(['/', '\\', ':'], "-")),
            Path::to_path_buf,
        )
    }

    fn layout(shared: bool) -> Fixture {
        crate::session_summary_cache::clear_session_summary_cache();
        let root = tempfile::tempdir().expect("root");
        let cwd = root.path().join("new").join("repo");
        let live = root.path().join("clone").join("repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        std::fs::create_dir_all(&live).expect("live");
        let sessions_root = root.path().join("sessions");
        let shared_dir = shared.then(|| root.path().join("shared"));
        let dir = |cwd: &Path| dir_for(&sessions_root, shared_dir.as_deref(), cwd);
        let gone = |name: &str| root.path().join("gone").join(name);
        let moved = write_session(
            &dir(&gone("repo")),
            &gone("repo"),
            "s1",
            Some(ROOT_COMMIT),
            "moved same repo",
        );
        let files = vec![
            moved.clone(),
            write_session(
                &dir(&gone("other")),
                &gone("other"),
                "s2",
                Some(OTHER_COMMIT),
                "x",
            ),
            write_session(
                &dir(&gone("unrecorded")),
                &gone("unrecorded"),
                "s3",
                None,
                "x",
            ),
            write_session(&dir(&live), &live, "s4", Some(ROOT_COMMIT), "x"),
            write_session(&dir(&cwd), &cwd, "s5", Some(ROOT_COMMIT), "x"),
        ];
        Fixture {
            cwd: cwd.to_string_lossy().into_owned(),
            old_cwd: gone("repo").to_string_lossy().into_owned(),
            sessions_root: shared_dir.unwrap_or(sessions_root),
            moved,
            files,
            _root: root,
        }
    }

    fn test_layout<'a>(fixture: &Fixture, resolve_moved: &'a dyn Fn(&str) -> String) -> Layout<'a> {
        Layout {
            sessions_root: fixture.sessions_root.to_string_lossy().into_owned(),
            own_dir: dir_for(&fixture.sessions_root, None, Path::new(&fixture.cwd))
                .to_string_lossy()
                .into_owned(),
            resolve_moved,
        }
    }

    fn identity_reader(
        cwd: String,
    ) -> impl Fn(String) -> std::future::Ready<Option<RepositoryIdentity>> {
        move |dir| std::future::ready((dir == cwd).then(recorded))
    }

    fn unchanged(path: &str) -> String {
        path.to_owned()
    }

    #[test]
    fn carries_the_recorded_repository_identity_on_every_listed_session() {
        let fixture = layout(false);
        let dir = Path::new(&fixture.moved).parent().expect("dir");
        let sessions = list_sessions_from_dir(&dir.to_string_lossy(), None, 0, None);
        assert_eq!(sessions[0].repository_identity, Some(recorded()));
        assert!(!sessions[0].moved);
    }

    #[tokio::test]
    async fn lists_only_same_repository_sessions_whose_recorded_path_is_gone_marked_moved() {
        let fixture = layout(false);
        let layout = test_layout(&fixture, &unchanged);
        let found = vanished_sessions(
            &fixture.cwd,
            None,
            identity_reader(fixture.cwd.clone()),
            &[VanishedKind::Moved],
            &layout,
        )
        .await
        .moved;
        assert_eq!(
            found.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            [fixture.moved.as_str()]
        );
        assert!(found[0].moved);
        assert_eq!(found[0].cwd, fixture.old_cwd);
    }

    #[tokio::test]
    async fn finds_them_in_a_shared_session_dir_too() {
        let fixture = layout(true);
        let shared = fixture.sessions_root.to_string_lossy().into_owned();
        let layout = test_layout(&fixture, &unchanged);
        let found = vanished_sessions(
            &fixture.cwd,
            Some(&shared),
            identity_reader(fixture.cwd.clone()),
            &[VanishedKind::Moved],
            &layout,
        )
        .await
        .moved;
        assert_eq!(
            found.iter().map(|s| s.path.as_str()).collect::<Vec<_>>(),
            [fixture.moved.as_str()]
        );
    }

    #[tokio::test]
    async fn marks_the_same_session_in_the_all_projects_list_and_nothing_else() {
        let fixture = layout(false);
        let all = crate::session_discovery::list_session_infos(&fixture.files, |_| {});
        let layout = test_layout(&fixture, &unchanged);
        let marked = mark_moved_sessions_in(
            &all,
            &fixture.cwd,
            identity_reader(fixture.cwd.clone()),
            &layout,
        )
        .await;
        assert_eq!(marked.len(), 5);
        let moved: Vec<_> = marked
            .iter()
            .filter(|s| s.moved)
            .map(|s| s.path.as_str())
            .collect();
        assert_eq!(moved, [fixture.moved.as_str()]);
    }

    #[tokio::test]
    async fn lists_nothing_when_the_current_directory_is_not_a_git_repository() {
        let fixture = layout(false);
        let layout = test_layout(&fixture, &unchanged);
        let none = |_: String| std::future::ready(None);
        let found =
            vanished_sessions(&fixture.cwd, None, none, &[VanishedKind::Moved], &layout).await;
        assert!(found.moved.is_empty());
    }

    #[tokio::test]
    async fn a_desktop_moved_session_is_here_unbadged_and_continues_when_newest() {
        let fixture = layout(false);
        let (cwd, old) = (fixture.cwd.clone(), fixture.old_cwd.clone());
        let to_here = move |path: &str| {
            if path == old {
                cwd.clone()
            } else {
                path.to_owned()
            }
        };
        let layout = test_layout(&fixture, &to_here);
        let local = list_sessions_from_dir(&layout.own_dir, None, 0, None);
        let listed = with_moved_sessions_in(
            local,
            &fixture.cwd,
            None,
            |_: String| std::future::ready(None),
            &layout,
        )
        .await;
        let row = listed
            .iter()
            .find(|s| s.path == fixture.moved)
            .expect("moved-here row");
        assert!(!row.moved, "a session moved here is this folder's own");
        assert_eq!(listed.len(), 2);

        let own = std::fs::File::options()
            .write(true)
            .open(&fixture.files[4])
            .expect("own");
        own.set_modified(std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .expect("age own");
        assert_eq!(
            moved_here_session_to_continue_in(&fixture.cwd, &layout.own_dir, &layout).await,
            Some(fixture.moved.clone())
        );
        let moved = std::fs::File::options()
            .write(true)
            .open(&fixture.moved)
            .expect("moved");
        moved
            .set_modified(std::time::SystemTime::UNIX_EPOCH)
            .expect("age moved");
        assert_eq!(
            moved_here_session_to_continue_in(&fixture.cwd, &layout.own_dir, &layout).await,
            None
        );
    }
}
