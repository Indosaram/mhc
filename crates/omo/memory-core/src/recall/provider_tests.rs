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
    repo.init(InitializeGitRepoOptions::default())
        .expect("init");
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

fn seeded_repo(dir: &std::path::Path, description: &str) -> GitMemoryRepo {
    let repo = GitMemoryRepo::open(dir, "recall-agent").expect("repo");
    repo.init(Some(InitializeGitRepoOptions {
        seed_files: vec![GitSeedFile {
            relative_path: "reference/topic.md".to_string(),
            content: format!("---\ndescription: {description}\n---\nBody.\n"),
        }],
        ..Default::default()
    }))
    .expect("init");
    repo
}

#[test]
fn shared_caches_hold_one_corpus_per_repository_even_through_another_spelling() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = seeded_repo(dir.path(), "Shared topic");
    let options = RecallCorpusCacheOptions {
        shared: true,
        ..Default::default()
    };
    let mut first = RecallCorpusCache::new(options.clone());
    let mut second = RecallCorpusCache::new(options);
    let a = first.load(&repo).expect("first load");
    let spelled = GitMemoryRepo::open(dir.path().join("."), "session-b").expect("repo");
    let b = second.load(&spelled).expect("second load");
    assert!(Arc::ptr_eq(&a, &b));
    first.settled();
    first.clear();
    let again = first.load(&repo).expect("reload");
    assert!(
        Arc::ptr_eq(&again, &b),
        "second cache still keeps the shared state alive"
    );
}

#[test]
fn a_private_cache_does_not_share() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = seeded_repo(dir.path(), "Private topic");
    let mut first = RecallCorpusCache::default();
    let mut second = RecallCorpusCache::default();
    let a = first.load(&repo).expect("first load");
    let b = second.load(&repo).expect("second load");
    assert!(!Arc::ptr_eq(&a, &b));
}
