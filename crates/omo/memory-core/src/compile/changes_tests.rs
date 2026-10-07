use super::*;
use crate::git::{GitCommitAuthor, GitMemoryRepo, GitSeedFile, InitializeGitRepoOptions};

fn author() -> GitCommitAuthor {
    GitCommitAuthor {
        agent_id: "changes".to_string(),
        author_name: "Changes".to_string(),
        author_email: None,
    }
}

fn fixture() -> (tempfile::TempDir, GitMemoryRepo) {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path(), "changes").expect("repo");
    repo.init(Some(InitializeGitRepoOptions {
        seed_files: vec![
            GitSeedFile {
                relative_path: "system/persona.md".to_string(),
                content: "---\ndescription: Persona\n---\nfirst\n".to_string(),
            },
            GitSeedFile {
                relative_path: "notes/n.md".to_string(),
                content: "---\ndescription: N\n---\nx\n".to_string(),
            },
        ],
        ..Default::default()
    }))
    .expect("init");
    (dir, repo)
}

#[test]
fn reports_added_and_updated_between_two_revisions() {
    let (_dir, repo) = fixture();
    let base = repo.head().expect("head").expect("base");
    std::fs::write(
        repo.dir.join("system/persona.md"),
        "---\ndescription: Persona\n---\nsecond\n",
    )
    .expect("edit body");
    std::fs::write(
        repo.dir.join("notes/extra.md"),
        "---\ndescription: Extra\n---\ny\n",
    )
    .expect("add");
    repo.commit_write(
        &["system/persona.md", "notes/extra.md"],
        "change",
        &author(),
    )
    .expect("commit");
    let head = repo.head().expect("head").expect("head");

    let changes = projected_changes_between(
        &repo,
        Some(base.as_str()),
        Some(head.as_str()),
        &ProjectedChangesOptions::default(),
    )
    .expect("changes");

    assert_eq!(changes.updated, vec!["system/persona.md".to_string()]);
    assert_eq!(changes.added, vec!["notes/extra.md".to_string()]);
    assert!(changes.removed.is_empty());
    assert!(!is_empty_projected_changes(&changes));
}

#[test]
fn a_non_system_path_edit_counts_only_on_add_or_remove() {
    let (_dir, repo) = fixture();
    let base = repo.head().expect("head").expect("base");
    std::fs::write(
        repo.dir.join("notes/n.md"),
        "---\ndescription: N\n---\nchanged\n",
    )
    .expect("edit note");
    repo.commit_write(&["notes/n.md"], "edit note", &author())
        .expect("commit");
    let head = repo.head().expect("head").expect("head");

    let changes = projected_changes_between(
        &repo,
        Some(base.as_str()),
        Some(head.as_str()),
        &ProjectedChangesOptions::default(),
    )
    .expect("changes");

    assert!(is_empty_projected_changes(&changes));
}

#[test]
fn a_removal_is_reported() {
    let (_dir, repo) = fixture();
    let base = repo.head().expect("head").expect("base");
    std::fs::remove_file(repo.dir.join("notes/n.md")).expect("remove");
    repo.commit_write(&["notes/n.md"], "remove note", &author())
        .expect("commit");
    let head = repo.head().expect("head").expect("head");

    let changes = projected_changes_between(
        &repo,
        Some(base.as_str()),
        Some(head.as_str()),
        &ProjectedChangesOptions::default(),
    )
    .expect("changes");

    assert_eq!(changes.removed, vec!["notes/n.md".to_string()]);
}

#[test]
fn a_commit_carrying_the_excluded_session_trailer_is_skipped() {
    let (_dir, repo) = fixture();
    let base = repo.head().expect("head").expect("base");
    std::fs::write(
        repo.dir.join("system/persona.md"),
        "---\ndescription: Persona\n---\nthird\n",
    )
    .expect("edit body");
    repo.commit_write(
        &["system/persona.md"],
        "session write\n\nOmo-Session: session-1",
        &author(),
    )
    .expect("commit");
    let head = repo.head().expect("head").expect("head");

    let excluded = projected_changes_between(
        &repo,
        Some(base.as_str()),
        Some(head.as_str()),
        &ProjectedChangesOptions {
            exclude_session_id: Some("session-1".to_string()),
        },
    )
    .expect("excluded changes");
    assert!(is_empty_projected_changes(&excluded));

    let included = projected_changes_between(
        &repo,
        Some(base.as_str()),
        Some(head.as_str()),
        &ProjectedChangesOptions::default(),
    )
    .expect("included changes");
    assert_eq!(included.updated, vec!["system/persona.md".to_string()]);
}

#[test]
fn identical_or_missing_revisions_report_no_changes() {
    let (_dir, repo) = fixture();
    let head = repo.head().expect("head").expect("head");
    assert!(is_empty_projected_changes(
        &projected_changes_between(
            &repo,
            Some(head.as_str()),
            Some(head.as_str()),
            &ProjectedChangesOptions::default()
        )
        .expect("same revision")
    ));
    assert!(is_empty_projected_changes(
        &projected_changes_between(
            &repo,
            None,
            None,
            &ProjectedChangesOptions::default()
        )
        .expect("missing head")
    ));
}

#[test]
fn revision_exists_reflects_the_repository_history() {
    let (_dir, repo) = fixture();
    let head = repo.head().expect("head").expect("head");
    assert!(revision_exists(&repo, &head).expect("existing revision"));
    assert!(
        !revision_exists(&repo, "0000000000000000000000000000000000000000")
            .expect("missing revision")
    );
}
