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
