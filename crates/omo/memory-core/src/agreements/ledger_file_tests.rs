use pretty_assertions::assert_eq;
use tempfile::tempdir;

use super::{
    agreement_items_of, known_agreements_of, read_agreements_at, read_agreements_worktree,
    render_agreements_file,
};
use crate::agreements::types::{AGREEMENTS_PATH, AgreementAuthor};
use crate::git::GitCommitAuthor;

const BODY: &str = "- [global] Be brief <!-- id: bbbbbbbbbb; by: agent; n: 1; since: 2026-01-01; last: 2026-01-01; status: active -->";

#[test]
fn render_creates_default_frontmatter_and_empty_body() {
    let items = agreement_items_of(Some(&format!("---\ndescription: x\n---\n{BODY}\n")));
    let rendered = render_agreements_file(None, &items).expect("render");
    assert!(rendered.starts_with("---\ndescription: Agreements - standing working agreements"));
    assert!(rendered.ends_with(&format!("{BODY}\n")));
    assert_eq!(
        render_agreements_file(None, &[]).expect("empty"),
        "---\ndescription: Agreements - standing working agreements with my person, with how often each was broken\n---\n"
    );
}

#[test]
fn render_keeps_frontmatter_and_unparsed_lines() {
    let existing = format!("---\ndescription: Mine\n---\n{BODY}\nfree text\n");
    let items = agreement_items_of(Some(&existing));
    let rendered = render_agreements_file(Some(&existing), &items).expect("render");
    // Upstream agreements/ledger-file.ts snapshots legacy dates into the event journal.
    let events = crate::agreements::events::serialize_agreement_events(
        &crate::agreements::events::legacy_agreement_events(&items),
    );
    assert_eq!(rendered, format!("{existing}{}\n", events.join("\n")));
}

#[test]
fn known_agreements_project_the_public_fields() {
    let items = agreement_items_of(Some(&format!("---\ndescription: x\n---\n{BODY}\n")));
    let known = known_agreements_of(&items);
    assert_eq!(known[0].id, "bbbbbbbbbb");
    assert_eq!(known[0].by, AgreementAuthor::Agent);
    assert_eq!(known[0].n, 1);
}

#[test]
fn reads_worktree_and_revision() {
    let dir = tempdir().expect("tempdir");
    let repo = crate::support::test_repo::init_test_repo(dir.path());
    assert_eq!(read_agreements_worktree(dir.path()).expect("read"), None);
    let head = repo.head().expect("head").expect("head");
    assert_eq!(read_agreements_at(&repo, &head).expect("at"), None);

    let content = format!("---\ndescription: x\n---\n{BODY}\n");
    std::fs::create_dir_all(dir.path().join("system")).expect("mkdir");
    std::fs::write(dir.path().join(AGREEMENTS_PATH), &content).expect("write");
    assert_eq!(
        read_agreements_worktree(dir.path()).expect("read"),
        Some(content.clone())
    );
    let author = GitCommitAuthor {
        agent_id: "a".to_string(),
        author_name: "A".to_string(),
        author_email: None,
    };
    repo.commit_write(&[AGREEMENTS_PATH], "add ledger", &author)
        .expect("commit");
    let head = repo.head().expect("head").expect("head");
    assert_eq!(read_agreements_at(&repo, &head).expect("at"), Some(content));
}

mod committed_cache {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use pretty_assertions::assert_eq;

    use super::super::read_agreements_at;
    use crate::agreements::types::AGREEMENTS_PATH;
    use crate::git::{GitExec, GitExecOptions, GitExecResult, GitMemoryRepo, GitMemoryRepoOptions};

    const FIRST: &str = "1111111111111111111111111111111111111111";

    /// The committed-ledger cache is process-wide; these tests count its git calls, so they must
    /// not evict each other's entries.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn serial() -> std::sync::MutexGuard<'static, ()> {
        SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[derive(Default)]
    struct FakeGit {
        calls: Mutex<Vec<Vec<String>>>,
        contents: Mutex<Vec<(String, String)>>,
        fail: AtomicBool,
    }

    impl GitExec for FakeGit {
        fn run(&self, argv: &[String], _: &GitExecOptions) -> std::io::Result<GitExecResult> {
            self.calls.lock().unwrap().push(argv.to_vec());
            if self.fail.load(Ordering::SeqCst) {
                return Ok(GitExecResult {
                    code: 128,
                    stdout: String::new(),
                    stderr: "unavailable object".into(),
                });
            }
            let revision = if argv[0] == "ls-tree" {
                argv[4].clone()
            } else {
                argv[1].split(':').next().unwrap_or_default().to_string()
            };
            let content = self
                .contents
                .lock()
                .unwrap()
                .iter()
                .find(|(rev, _)| *rev == revision)
                .map(|(_, content)| content.clone());
            let stdout = match (argv[0].as_str(), content) {
                ("ls-tree", Some(_)) => format!("{AGREEMENTS_PATH}\0"),
                ("ls-tree", None) => String::new(),
                (_, content) => content.unwrap_or_default(),
            };
            Ok(GitExecResult {
                code: 0,
                stdout,
                stderr: String::new(),
            })
        }
    }

    fn open(dir: &std::path::Path, git: &Arc<FakeGit>) -> GitMemoryRepo {
        GitMemoryRepo::new(GitMemoryRepoOptions {
            dir: dir.to_path_buf(),
            agent_id: "ledger-test".into(),
            exec: Some(Arc::clone(git) as Arc<dyn GitExec>),
            install_hooks: None,
        })
        .expect("repo")
    }

    fn calls(git: &FakeGit) -> Vec<Vec<String>> {
        git.calls.lock().unwrap().clone()
    }

    #[test]
    fn a_full_object_id_is_read_once_through_a_scoped_tree_listing() {
        let _serial = serial();
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        let git = Arc::new(FakeGit::default());
        git.contents
            .lock()
            .unwrap()
            .push((FIRST.into(), "first ledger\n".into()));
        // When
        let first = read_agreements_at(&open(root.path(), &git), FIRST).expect("first");
        let before = calls(&git).len();
        let again = read_agreements_at(&open(root.path(), &git), FIRST).expect("again");
        // Then
        assert_eq!(first.as_deref(), Some("first ledger\n"));
        assert_eq!(again, first);
        assert_eq!(calls(&git).len(), before, "the cached read spawns no git");
        let listing = &calls(&git)[0];
        assert_eq!(listing[0], "ls-tree");
        assert_eq!(listing[listing.len() - 2..], ["--", AGREEMENTS_PATH]);
    }

    #[test]
    fn a_mutable_revision_and_a_failed_read_are_never_cached() {
        let _serial = serial();
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        let git = Arc::new(FakeGit::default());
        git.contents
            .lock()
            .unwrap()
            .push(("HEAD".into(), "before\n".into()));
        assert_eq!(
            read_agreements_at(&open(root.path(), &git), "HEAD").expect("before"),
            Some("before\n".into())
        );
        // When
        git.contents.lock().unwrap()[0].1 = "after\n".into();
        // Then
        assert_eq!(
            read_agreements_at(&open(root.path(), &git), "HEAD").expect("after"),
            Some("after\n".into())
        );

        // Given
        git.fail.store(true, Ordering::SeqCst);
        assert!(read_agreements_at(&open(root.path(), &git), FIRST).is_err());
        // When
        git.fail.store(false, Ordering::SeqCst);
        git.contents
            .lock()
            .unwrap()
            .push((FIRST.into(), "first ledger\n".into()));
        // Then
        assert_eq!(
            read_agreements_at(&open(root.path(), &git), FIRST).expect("retry"),
            Some("first ledger\n".into())
        );
    }

    #[test]
    fn an_old_object_id_is_reread_after_the_bounded_cache_evicts_it() {
        let _serial = serial();
        // Given
        let root = tempfile::tempdir().expect("tempdir");
        let git = Arc::new(FakeGit::default());
        git.contents
            .lock()
            .unwrap()
            .push((FIRST.into(), "first ledger\n".into()));
        read_agreements_at(&open(root.path(), &git), FIRST).expect("first");
        for index in 0..40u32 {
            read_agreements_at(&open(root.path(), &git), &format!("{index:040x}")).expect("other");
        }
        let before = calls(&git).len();
        // When
        let value = read_agreements_at(&open(root.path(), &git), FIRST).expect("revisit");
        // Then
        assert_eq!(value.as_deref(), Some("first ledger\n"));
        assert_eq!(calls(&git).len() - before, 2);
    }
}
