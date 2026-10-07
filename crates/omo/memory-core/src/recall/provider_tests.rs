use super::*;
use crate::git::{GitMemoryRepo, GitSeedFile, InitializeGitRepoOptions};

#[test]
fn only_markdown_outside_the_root_system_tree_is_recallable() {
    assert!(is_recall_candidate_path("notes/fact.md"));
    assert!(is_recall_candidate_path("reference/system/deploy.md"));
    assert!(!is_recall_candidate_path("system/persona.md"));
    assert!(!is_recall_candidate_path("notes/fact.txt"));
}

#[test]
fn a_repository_without_a_commit_yields_an_empty_corpus() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path(), "recall-agent").expect("repo");
    repo.init(InitializeGitRepoOptions::default()).expect("init");
    let corpus = load_recall_corpus(&repo).expect("corpus");
    assert!(corpus.revision.is_some());
    assert!(corpus.documents.is_empty());
}

#[test]
fn the_corpus_reads_committed_files_and_skips_the_system_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path(), "recall-agent").expect("repo");
    repo.init(Some(InitializeGitRepoOptions {
        seed_files: vec![
            GitSeedFile {
                relative_path: "notes/fact.md".to_string(),
                content: "---\ndescription: A fact\n---\nbody text\n".to_string(),
            },
            GitSeedFile {
                relative_path: "system/persona.md".to_string(),
                content: "---\ndescription: Persona\n---\nsecret\n".to_string(),
            },
            GitSeedFile {
                relative_path: "reference/system/deploy.md".to_string(),
                content: "---\ndescription: Deploy\n---\nrollback\n".to_string(),
            },
        ],
        ..Default::default()
    }))
    .expect("init");

    let corpus = load_recall_corpus(&repo).expect("corpus");
    let paths: Vec<&str> = corpus.documents.iter().map(|d| d.path.as_str()).collect();
    assert_eq!(paths, vec!["notes/fact.md", "reference/system/deploy.md"]);
    assert_eq!(corpus.documents[0].description, "A fact");
    assert_eq!(corpus.documents[0].body, "body text\n");
}

#[test]
fn a_file_without_valid_frontmatter_is_skipped_not_fatal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path(), "recall-agent").expect("repo");
    repo.init(Some(InitializeGitRepoOptions {
        seed_files: vec![
            GitSeedFile {
                relative_path: "notes/broken.md".to_string(),
                content: "no frontmatter at all\n".to_string(),
            },
            GitSeedFile {
                relative_path: "notes/good.md".to_string(),
                content: "---\ndescription: Good\n---\nkept\n".to_string(),
            },
        ],
        ..Default::default()
    }))
    .expect("init");

    let corpus = load_recall_corpus(&repo).expect("corpus");
    assert_eq!(corpus.documents.len(), 1);
    assert_eq!(corpus.documents[0].path, "notes/good.md");
}

#[test]
fn uncommitted_edits_never_leak_into_the_corpus() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path(), "recall-agent").expect("repo");
    repo.init(Some(InitializeGitRepoOptions {
        seed_files: vec![GitSeedFile {
            relative_path: "notes/fact.md".to_string(),
            content: "---\ndescription: A fact\n---\ncommitted\n".to_string(),
        }],
        ..Default::default()
    }))
    .expect("init");
    std::fs::write(
        repo.dir.join("notes/fact.md"),
        "---\ndescription: A fact\n---\nuncommitted\n",
    )
    .expect("edit working tree");

    let corpus = load_recall_corpus(&repo).expect("corpus");
    assert_eq!(corpus.documents[0].body, "committed\n");
}
