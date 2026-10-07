use super::{parse_cat_file_batch, parse_ls_tree_blobs, parse_ls_tree_sized};
use crate::git::repo_types::{GitTreeBlobEntry, GitTreeSizedEntry};
use crate::git::{GitMemoryRepo, GitSeedFile, InitializeGitRepoOptions};

#[test]
fn parse_ls_tree_sized_reads_byte_sizes_and_skips_tree_entries() {
    let stdout = concat!(
        "100644 blob aaa111 12\tsystem/persona.md\u{0}",
        "040000 tree bbb222 -\tsystem\u{0}",
    );
    assert_eq!(
        parse_ls_tree_sized(stdout),
        vec![GitTreeSizedEntry {
            path: "system/persona.md".to_string(),
            bytes: 12,
        }]
    );
}

#[test]
fn parse_ls_tree_blobs_keeps_only_blobs() {
    let stdout = concat!(
        "100644 blob aaa111\tsystem/persona.md\u{0}",
        "040000 tree bbb222\tsystem\u{0}",
    );
    assert_eq!(
        parse_ls_tree_blobs(stdout),
        vec![GitTreeBlobEntry {
            path: "system/persona.md".to_string(),
            oid: "aaa111".to_string(),
        }]
    );
}

#[test]
fn parse_cat_file_batch_reads_blob_content_and_skips_missing_oids() {
    let output = b"aaa111 blob 5\nhello\nbbb222 missing\n";
    let blobs = parse_cat_file_batch(output).expect("parse");
    assert_eq!(blobs.get("aaa111").map(String::as_str), Some("hello"));
    assert!(blobs.get("bbb222").is_none());
}

#[test]
fn parse_cat_file_batch_rejects_a_truncated_record() {
    let output = b"aaa111 blob 99\nshort\n";
    assert!(parse_cat_file_batch(output).is_err());
}

#[test]
fn real_repo_readers_report_sizes_and_blob_content_by_oid() {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = GitMemoryRepo::open(dir.path(), "tree-agent").expect("repo");
    let persona = "---\ndescription: Persona\n---\nbody\n";
    let note = "---\ndescription: N\n---\nx\n";
    repo.init(Some(InitializeGitRepoOptions {
        seed_files: vec![
            GitSeedFile {
                relative_path: "system/persona.md".to_string(),
                content: persona.to_string(),
            },
            GitSeedFile {
                relative_path: "notes/n.md".to_string(),
                content: note.to_string(),
            },
        ],
        ..Default::default()
    }))
    .expect("init");

    let sized = repo.ls_tree_sized(None).expect("sized");
    let entry = sized
        .iter()
        .find(|entry| entry.path == "system/persona.md")
        .expect("persona entry");
    assert_eq!(entry.bytes, persona.len() as u64);

    let blobs = repo.ls_tree_blobs(None).expect("blobs");
    let oids: Vec<String> = blobs
        .iter()
        .filter(|entry| entry.path == "notes/n.md")
        .map(|entry| entry.oid.clone())
        .collect();
    assert_eq!(oids.len(), 1);
    let content = repo.read_blobs(&oids).expect("read blobs");
    assert_eq!(content.get(&oids[0]).map(String::as_str), Some(note));

    assert!(repo.read_blobs(&[]).expect("empty request").is_empty());
    assert!(
        repo.read_blobs(&["0000000000000000000000000000000000000000".to_string()])
            .expect("missing oid")
            .is_empty()
    );
}
