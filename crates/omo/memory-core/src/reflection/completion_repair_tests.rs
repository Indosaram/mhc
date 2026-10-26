use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Arc;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use crate::git::exec::{GitExec, system_git_exec};
use crate::git::{GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile, InitializeGitRepoOptions};

use crate::reflection::completion_repair::{
    DroppedEntry, commit_count, describe_dropped_entries, read_dropped_trailers,
    squash_without_paths,
};

const GOOD: &str = "---\ndescription: A good note\n---\n\nbody\n";
const LEAK: &str = "---\ndescription: Leak\n---\n\nleak body\n";

struct Fixture {
    _temp: TempDir,
    exec: Arc<dyn GitExec>,
    dir: std::path::PathBuf,
    base: String,
}

fn fixture(seeds: &[(&str, &str)]) -> Fixture {
    let temp = TempDir::new().expect("temp dir");
    let dir = temp.path().join("memory");
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: dir.clone(),
        agent_id: "agent-repair".to_string(),
        exec: None,
        install_hooks: None,
    })
    .expect("open repo");
    repo.init(InitializeGitRepoOptions {
        seed_files: seeds
            .iter()
            .map(|(path, content)| GitSeedFile {
                relative_path: (*path).to_string(),
                content: (*content).to_string(),
            })
            .collect(),
        author_name: Some("Repair Agent".to_string()),
        install_hooks: None,
    })
    .expect("repo init");
    let base = repo.head().expect("head").expect("base commit");
    Fixture {
        _temp: temp,
        exec: system_git_exec(),
        dir,
        base,
    }
}

fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (path, content) in files {
        let full = dir.join(path);
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(full, content).expect("write file");
    }
}

fn commit_all(exec: &dyn GitExec, dir: &Path, message: &str) -> String {
    let add = exec.run_in(dir, &["add", "-A"]).expect("git add");
    assert_eq!(add.code, 0, "{}", add.stderr);
    let commit = exec
        .run_in(
            dir,
            &[
                "-c",
                "user.email=fixture@example.com",
                "-c",
                "user.name=fixture",
                "commit",
                "-qm",
                message,
            ],
        )
        .expect("git commit");
    assert_eq!(commit.code, 0, "{}", commit.stderr);
    exec.run_in(dir, &["rev-parse", "HEAD"])
        .expect("git rev-parse")
        .stdout
        .trim()
        .to_string()
}

fn show(dir: &Path, exec: &dyn GitExec, spec: &str) -> String {
    let result = exec.run_in(dir, &["cat-file", "blob", spec]).expect("cat-file");
    assert_eq!(result.code, 0, "{}", result.stderr);
    result.stdout
}

#[test]
fn given_one_dropped_entry_when_described_then_it_is_singular_and_redacted() {
    let note = describe_dropped_entries(&[DroppedEntry {
        path: "reference/token=abc123456.md".to_string(),
        reason: "secret_like_content: reference/token=*** (credential_assignment, file name)".to_string(),
    }]);

    assert!(note.starts_with("Dropped 1 invalid memory entry: "), "{note}");
    assert!(!note.contains("abc123456"), "{note}");
}

#[test]
fn given_two_dropped_entries_when_described_then_it_is_plural_and_redacted() {
    let note = describe_dropped_entries(&[
        DroppedEntry {
            path: "reference/leak.md".to_string(),
            reason: "secret_like_content: reference/leak.md (pem_block)".to_string(),
        },
        DroppedEntry {
            path: "reference/ghp_Ab3dEf5hJ7kL9mN1pQ3rS5tU7vW9xY1zB3C5.md".to_string(),
            reason: "secret_like_content: reference/*** (vendor_token, file name)".to_string(),
        },
    ]);

    assert!(note.starts_with("Dropped 2 invalid memory entries: "), "{note}");
    assert!(!note.contains("Ab3dEf5hJ7kL9mN1pQ3rS5tU7vW9xY1zB3C5"), "{note}");
    assert!(note.contains("reference/leak.md"), "{note}");
}

#[test]
fn given_a_multi_commit_branch_when_counted_then_every_commit_counts() {
    let fixture = fixture(&[("reference/keep.md", GOOD)]);
    write_files(&fixture.dir, &[("reference/keep.md", LEAK)]);
    commit_all(fixture.exec.as_ref(), &fixture.dir, "first");
    write_files(&fixture.dir, &[("reference/keep.md", GOOD)]);
    let tip = commit_all(fixture.exec.as_ref(), &fixture.dir, "second");

    let count = commit_count(
        fixture.exec.as_ref(),
        &fixture.dir,
        &fixture.base,
        &tip,
    )
    .expect("commit count");

    assert_eq!(count, 2);
}

#[test]
fn given_a_dropped_path_when_squashed_then_it_returns_to_its_base_content_and_the_rest_is_kept() {
    let fixture = fixture(&[("reference/keep.md", GOOD), ("reference/leak.md", LEAK)]);
    let edited = GOOD.replace("body", "edited body");
    write_files(
        &fixture.dir,
        &[
            ("reference/keep.md", edited.as_str()),
            ("reference/leak.md", LEAK.replace("leak body", "new leak").as_str()),
        ],
    );
    let tip = commit_all(fixture.exec.as_ref(), &fixture.dir, "edit both");
    let dropped = BTreeSet::from(["reference/leak.md".to_string()]);

    let squashed = squash_without_paths(
        fixture.exec.as_ref(),
        &fixture.dir,
        &fixture.base,
        &tip,
        &dropped,
        &[],
    )
    .expect("squash");

    assert_ne!(squashed, tip);
    assert_eq!(
        show(&fixture.dir, fixture.exec.as_ref(), &format!("{squashed}:reference/keep.md")),
        edited
    );
    assert_eq!(
        show(&fixture.dir, fixture.exec.as_ref(), &format!("{squashed}:reference/leak.md")),
        LEAK
    );
    assert_eq!(
        commit_count(fixture.exec.as_ref(), &fixture.dir, &fixture.base, &squashed).expect("count"),
        1
    );
    let parent = fixture
        .exec
        .run_in(&fixture.dir, &["rev-parse", &format!("{squashed}^")])
        .expect("parent");
    assert_eq!(parent.stdout.trim(), fixture.base);
}

#[test]
fn given_every_entry_dropped_when_squashed_then_the_base_is_returned() {
    let fixture = fixture(&[("reference/keep.md", GOOD)]);
    write_files(&fixture.dir, &[("reference/leak.md", LEAK)]);
    let tip = commit_all(fixture.exec.as_ref(), &fixture.dir, "add leak");
    let dropped = BTreeSet::from(["reference/leak.md".to_string()]);

    let squashed = squash_without_paths(
        fixture.exec.as_ref(),
        &fixture.dir,
        &fixture.base,
        &tip,
        &dropped,
        &[DroppedEntry {
            path: "reference/leak.md".to_string(),
            reason: "secret_like_content: reference/leak.md (pem_block)".to_string(),
        }],
    )
    .expect("squash");

    assert_eq!(squashed, fixture.base);
}

#[test]
fn given_a_dropped_rename_destination_when_squashed_then_the_source_is_restored() {
    let fixture = fixture(&[("notes-dump.md", "# Title\n\nno frontmatter\n")]);
    std::fs::create_dir_all(fixture.dir.join("reference")).expect("reference dir");
    let moved = fixture
        .exec
        .run_in(&fixture.dir, &["mv", "notes-dump.md", "reference/notes-dump.md"])
        .expect("git mv");
    assert_eq!(moved.code, 0, "{}", moved.stderr);
    let tip = commit_all(fixture.exec.as_ref(), &fixture.dir, "move it");
    let dropped = BTreeSet::from(["reference/notes-dump.md".to_string()]);

    let squashed = squash_without_paths(
        fixture.exec.as_ref(),
        &fixture.dir,
        &fixture.base,
        &tip,
        &dropped,
        &[],
    )
    .expect("squash");

    assert_eq!(
        show(&fixture.dir, fixture.exec.as_ref(), &format!("{squashed}:notes-dump.md")),
        "# Title\n\nno frontmatter\n"
    );
    let moved_back = fixture
        .exec
        .run_in(&fixture.dir, &["cat-file", "-e", &format!("{squashed}:reference/notes-dump.md")])
        .expect("cat-file -e");
    assert_ne!(moved_back.code, 0);
}

// Unix only: the fixture path contains `|`, which Windows forbids in file names.
#[cfg(unix)]
#[test]
fn given_a_recorded_drop_when_the_tip_is_read_back_then_the_entries_round_trip_verbatim() {
    let fixture = fixture(&[("reference/keep.md", GOOD)]);
    // Match upstream completion-validation-partial.test.ts: keep a valid change so the
    // repaired tip has a commit on which the dropped-entry trailer can be recorded.
    write_files(&fixture.dir, &[("reference/a | b.md", LEAK), ("reference/good.md", GOOD)]);
    let tip = commit_all(fixture.exec.as_ref(), &fixture.dir, "add leak");
    let dropped = BTreeSet::from(["reference/a | b.md".to_string()]);
    let record = vec![DroppedEntry {
        path: "reference/a | b.md".to_string(),
        reason: "invalid frontmatter: 'description' exceeds 1024 characters (1100)".to_string(),
    }];

    let squashed = squash_without_paths(
        fixture.exec.as_ref(),
        &fixture.dir,
        &fixture.base,
        &tip,
        &dropped,
        &record,
    )
    .expect("squash");
    let read_back = read_dropped_trailers(fixture.exec.as_ref(), &fixture.dir, &fixture.base, &squashed)
        .expect("read trailers");

    assert_eq!(read_back, record);
}

#[test]
fn given_a_tip_without_recorded_drops_when_read_back_then_nothing_is_reported() {
    let fixture = fixture(&[("reference/keep.md", GOOD)]);
    write_files(&fixture.dir, &[("reference/keep.md", GOOD.replace("body", "edited").as_str())]);
    let tip = commit_all(fixture.exec.as_ref(), &fixture.dir, "edit");

    let read_back = read_dropped_trailers(fixture.exec.as_ref(), &fixture.dir, &fixture.base, &tip)
        .expect("read trailers");

    assert_eq!(read_back, Vec::new());
}
