//! Port of senpi `core/resume-target.ts` (senpi#2184, senpi#2990).
use std::future::Future;
use std::path::Path;

use crate::paths::{PathInputOptions, resolve_path};
use crate::session_cwd::{read_session_cwd, rebind_session_file};
use crate::session_move::bundle::RepositoryIdentity;
use crate::session_rebind::classify_session_repository;

/// senpi `ResumeTarget`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResumeTarget {
    pub path: String,
    pub rebound: bool,
}

fn resolved(path: &str) -> String {
    resolve_path(path, path, &PathInputOptions::default())
}

/// Where a session picked for resuming should be opened from, reading identities with `git`.
pub async fn resolve_resume_target<C, CFut>(
    session_path: &str,
    cwd: &str,
    session_dir: Option<&str>,
    confirm: C,
) -> std::io::Result<ResumeTarget>
where
    C: FnOnce(String) -> CFut,
    CFut: Future<Output = bool>,
{
    resolve_resume_target_with(session_path, cwd, session_dir, confirm, |dir| async move {
        crate::repository_identity::read_repository_identity(&dir).await
    })
    .await
}

/// senpi `resolveResumeTarget` with an injected identity reader (`ResumeTargetChoice.readIdentity`).
/// A session of the current repository recorded at another path is offered the in-place rebind;
/// declining, another repository, or the current directory's own session open the picked file
/// unchanged. Rebind failures propagate (a held session names the process still holding it).
pub async fn resolve_resume_target_with<C, CFut, R, RFut>(
    session_path: &str,
    cwd: &str,
    session_dir: Option<&str>,
    confirm: C,
    read_identity: R,
) -> std::io::Result<ResumeTarget>
where
    C: FnOnce(String) -> CFut,
    CFut: Future<Output = bool>,
    R: Fn(String) -> RFut,
    RFut: Future<Output = Option<RepositoryIdentity>>,
{
    resolve_resume_target_in(
        session_path,
        cwd,
        session_dir,
        confirm,
        read_identity,
        crate::session_manager::resolve_moved_session_path,
    )
    .await
}

/// `resolve_moved` is `resolveMovedPath`; tests pass the same resolver bound to a temporary home.
async fn resolve_resume_target_in<C, CFut, R, RFut>(
    session_path: &str,
    cwd: &str,
    session_dir: Option<&str>,
    confirm: C,
    read_identity: R,
    resolve_moved: impl Fn(&str) -> String,
) -> std::io::Result<ResumeTarget>
where
    C: FnOnce(String) -> CFut,
    CFut: Future<Output = bool>,
    R: Fn(String) -> RFut,
    RFut: Future<Output = Option<RepositoryIdentity>>,
{
    let unchanged = ResumeTarget {
        path: session_path.to_owned(),
        rebound: false,
    };
    let Ok(Some(session_cwd)) = read_session_cwd(session_path).await else {
        return Ok(unchanged);
    };
    // A session whose cwd the OmO desktop moved here is this directory's own session, not one to
    // rebind (senpi#2990).
    if resolved(&resolve_moved(&session_cwd)) == resolved(cwd) {
        return Ok(unchanged);
    }
    let matched = classify_session_repository(session_path, &session_cwd, cwd, read_identity).await;
    if matched != "same" || !confirm(session_cwd).await {
        return Ok(unchanged);
    }
    let path = rebind_session_file(
        Path::new(session_path),
        Path::new(cwd),
        session_dir.map(Path::new),
        None,
    )
    .await?;
    Ok(ResumeTarget {
        path: path.to_string_lossy().into_owned(),
        rebound: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn recorded() -> RepositoryIdentity {
        RepositoryIdentity {
            root_commits: vec!["c".repeat(40)],
            origin_url: None,
        }
    }

    fn write_session(dir: &Path, cwd: &Path, id: &str) -> String {
        std::fs::create_dir_all(dir).expect("session dir");
        let file = dir.join(format!("2026-09-27_{id}.jsonl"));
        let lines = [
            json!({"type": "session", "version": 3, "id": id, "timestamp": "2026-09-27T00:00:00.000Z", "cwd": cwd}).to_string(),
            json!({"type": "custom", "customType": "repository-identity", "data": {"rootCommits": ["c".repeat(40)]}}).to_string(),
        ];
        std::fs::write(&file, lines.join("\n") + "\n").expect("session file");
        file.to_string_lossy().into_owned()
    }

    struct SameRepoElsewhere {
        _root: tempfile::TempDir,
        old_cwd: String,
        cwd: String,
        target_dir: std::path::PathBuf,
        session_file: String,
    }

    fn same_repo_elsewhere() -> SameRepoElsewhere {
        let root = tempfile::tempdir().expect("root");
        let old_cwd = root.path().join("old/repo");
        let cwd = root.path().join("new/repo");
        std::fs::create_dir_all(&cwd).expect("cwd");
        let session_file = write_session(
            &root.path().join("sessions/old"),
            &old_cwd,
            "0197f6e4-4cf9-7f44-a2d8-f8f7f49ee9d3",
        );
        SameRepoElsewhere {
            old_cwd: old_cwd.to_string_lossy().into_owned(),
            cwd: cwd.to_string_lossy().into_owned(),
            target_dir: root.path().join("sessions/new"),
            session_file,
            _root: root,
        }
    }

    fn same(_: String) -> std::future::Ready<Option<RepositoryIdentity>> {
        std::future::ready(Some(recorded()))
    }

    #[tokio::test]
    async fn rebinds_a_same_repository_session_recorded_elsewhere_when_the_user_confirms() {
        let fixture = same_repo_elsewhere();
        let mut asked = Vec::new();
        let target_dir = fixture.target_dir.to_string_lossy().into_owned();
        let target = resolve_resume_target_with(
            &fixture.session_file,
            &fixture.cwd,
            Some(&target_dir),
            |session_cwd| {
                asked.push(session_cwd);
                std::future::ready(true)
            },
            same,
        )
        .await
        .expect("resume target");
        assert_eq!(asked, [fixture.old_cwd.clone()]);
        assert!(target.rebound);
        assert_eq!(
            Path::new(&target.path).parent().expect("dir").file_name(),
            fixture.target_dir.file_name()
        );
        assert!(!Path::new(&fixture.session_file).exists());
    }

    #[tokio::test]
    async fn opens_the_session_where_it_is_when_the_user_declines() {
        let fixture = same_repo_elsewhere();
        let target = resolve_resume_target_with(
            &fixture.session_file,
            &fixture.cwd,
            None,
            |_| std::future::ready(false),
            same,
        )
        .await
        .expect("resume target");
        assert_eq!(
            target,
            ResumeTarget {
                path: fixture.session_file.clone(),
                rebound: false
            }
        );
        assert!(Path::new(&fixture.session_file).exists());
    }

    #[tokio::test]
    async fn never_asks_for_a_different_repository_or_for_the_current_directorys_own_session() {
        let fixture = same_repo_elsewhere();
        let local = write_session(
            &fixture.target_dir,
            Path::new(&fixture.cwd),
            "0197f6e4-0000-7000-8000-00000000000a",
        );
        let cwd = fixture.cwd.clone();
        let other = move |dir: String| {
            std::future::ready((dir == cwd).then(|| RepositoryIdentity {
                root_commits: vec!["d".repeat(40)],
                origin_url: None,
            }))
        };
        let must_not_ask = |_: String| -> std::future::Ready<bool> { panic!("must not ask") };
        let target = resolve_resume_target_with(
            &fixture.session_file,
            &fixture.cwd,
            None,
            must_not_ask,
            other,
        )
        .await
        .expect("different repository");
        assert_eq!(target.path, fixture.session_file);
        assert!(!target.rebound);
        let target = resolve_resume_target_with(&local, &fixture.cwd, None, must_not_ask, same)
            .await
            .expect("own session");
        assert_eq!(
            target,
            ResumeTarget {
                path: local,
                rebound: false
            }
        );
    }

    #[tokio::test]
    async fn reports_the_process_that_still_holds_the_session_instead_of_moving_it() {
        let fixture = same_repo_elsewhere();
        let target_dir = fixture.target_dir.to_string_lossy().into_owned();
        let hold = crate::schedule::runner_lease::hold_session_file(
            &std::fs::canonicalize(&fixture.session_file).expect("real session path"),
            "0197f6e4-4cf9-7f44-a2d8-f8f7f49ee9d3",
            &fixture.old_cwd,
            true,
        )
        .expect("hold");
        let error = resolve_resume_target_with(
            &fixture.session_file,
            &fixture.cwd,
            Some(&target_dir),
            |_| std::future::ready(true),
            same,
        )
        .await
        .expect_err("held");
        let held = error
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<crate::session_hold_errors::SessionHeldError>())
            .expect("SessionHeldError");
        assert_eq!(held.holders.len(), 1);
        assert_eq!(held.holders[0].pid, std::process::id());
        drop(hold);
        assert!(Path::new(&fixture.session_file).exists());
    }

    // senpi#2990: test/suite/regressions/issue-2990-moved-session-open.test.ts
    #[tokio::test]
    async fn resume_of_the_moved_session_from_the_new_folder_opens_it_as_is_without_a_rebind_question()
     {
        #[cfg(unix)]
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
        const HOME_ID: &str = "0199f0d4-0000-7000-8000-000000000000";
        let home = tempfile::tempdir().expect("home");
        let home_path = std::fs::canonicalize(home.path()).expect("real home");
        let old_root = home_path.join(".t3");
        let new_root = home_path.join(".omo/desktop");
        let old_worktree = old_root.join("worktrees/app/w1");
        let new_worktree = new_root.join("worktrees/app/w1");
        let new_sessions = new_root.join("userdata/omo-sessions");
        fn make_dir(dir: &Path) {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(dir).expect("dir");
        }
        fn write_private(file: &Path, body: String) {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            options.mode(0o600);
            std::io::Write::write_all(&mut options.open(file).expect("open"), body.as_bytes())
                .expect("write");
        }
        for dir in [&old_root, &new_worktree, &new_sessions] {
            make_dir(dir);
        }
        write_private(
            &new_root.join("omo-desktop-home.json"),
            json!({"kind": "omo-desktop-data-home", "schemaVersion": 1, "appId": "com.omo.desktop", "homeId": HOME_ID}).to_string(),
        );
        write_private(
            &old_root.join("omo-desktop-moved.json"),
            json!({"kind": "omo-desktop-moved", "schemaVersion": 1, "movedTo": new_root, "homeId": HOME_ID,
                "movedAt": "2026-10-07T00:00:00.000Z", "byVersion": "0.0.0-test",
                "moved": ["worktrees/app/w1", "userdata/omo-sessions"]})
            .to_string(),
        );
        let session_file = new_sessions.join("s.jsonl");
        let entries = [
            json!({"type": "session", "version": 3, "id": "0199f0d4-2990-7000-8000-000000000001", "timestamp": "2026-10-07T00:00:00.000Z", "cwd": old_worktree}).to_string(),
            json!({"type": "custom", "id": "r1", "parentId": null, "timestamp": "2026-10-07T00:00:01.000Z", "customType": "repository-identity", "data": {"rootCommits": ["c".repeat(40)]}}).to_string(),
        ];
        std::fs::write(&session_file, entries.join("\n") + "\n").expect("session");
        let bytes = std::fs::read(&session_file).expect("bytes");
        let session_file = session_file.to_string_lossy().into_owned();

        let target = resolve_resume_target_in(
            &session_file,
            &new_worktree.to_string_lossy(),
            None,
            |session_cwd: String| -> std::future::Ready<bool> {
                panic!("asked to rebind a session recorded at {session_cwd}")
            },
            same,
            |path| {
                crate::schedule::moved_paths::resolve_in_home(Path::new(path), &home_path)
                    .to_string_lossy()
                    .into_owned()
            },
        )
        .await
        .expect("resume target");

        assert_eq!(
            target,
            ResumeTarget {
                path: session_file.clone(),
                rebound: false
            }
        );
        assert_eq!(std::fs::read(&session_file).expect("unchanged"), bytes);
    }
}
