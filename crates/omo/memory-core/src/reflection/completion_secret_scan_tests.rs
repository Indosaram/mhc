use std::path::{Path, PathBuf};
use std::sync::Arc;

use pretty_assertions::assert_eq;
use tempfile::TempDir;

use crate::git::exec::{GitExec, system_git_exec};
use crate::git::{GitMemoryRepo, GitMemoryRepoOptions, GitSeedFile, InitializeGitRepoOptions};

use crate::reflection::completion_secret_scan::{
    SecretLikeFailure, find_secret_like_failure, find_secret_like_failures,
};

const PEM: &str = "-----BEGIN RSA PRIVATE KEY-----\nMIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSk\n-----END RSA PRIVATE KEY-----\n";
const GHP: &str = "ghp_Ab3dEf5hJ7kL9mN1pQ3rS5tU7vW9xY1zB3C5";
const XOXB: &str = "xoxb-1234567890abcdefghij";

struct Fixture {
    _temp: TempDir,
    exec: Arc<dyn GitExec>,
    dir: PathBuf,
    base: String,
}

fn fixture(seeds: &[(&str, &str)]) -> Fixture {
    let temp = TempDir::new().expect("temp dir");
    let dir = temp.path().join("memory");
    let repo = GitMemoryRepo::new(GitMemoryRepoOptions {
        dir: dir.clone(),
        agent_id: "agent-secret".to_string(),
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
        author_name: Some("Secret Agent".to_string()),
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

fn git(exec: &dyn GitExec, dir: &Path, args: &[&str]) -> String {
    let result = exec.run_in(dir, args).expect("git run");
    assert_eq!(result.code, 0, "git {}: {}", args.join(" "), result.stderr);
    result.stdout
}

fn write_at(dir: &Path, path: &str, content: &str) {
    let full = dir.join(path);
    if let Some(parent) = full.parent() {
        std::fs::create_dir_all(parent).expect("create parent");
    }
    std::fs::write(full, content).expect("write file");
}

fn commit_paths(exec: &dyn GitExec, dir: &Path, paths: &[&str], message: &str) -> String {
    let mut add: Vec<&str> = vec!["add", "-A", "--"];
    add.extend(paths);
    git(exec, dir, &add);
    git(
        exec,
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
    );
    git(exec, dir, &["rev-parse", "HEAD"]).trim().to_string()
}

fn failures(fixture: &Fixture, tip: &str) -> Vec<SecretLikeFailure> {
    find_secret_like_failures(
        fixture.exec.as_ref(),
        &fixture.dir,
        &fixture.base,
        tip,
    )
    .expect("secret scan")
}

#[test]
fn given_a_changed_file_with_a_pem_block_when_scanned_then_the_detail_names_the_class_and_short_sha() {
    let fixture = fixture(&[]);
    write_at(&fixture.dir, "reference/y.md", &format!("---\ndescription: Y\n---\n\n{PEM}"));
    let sha = commit_paths(fixture.exec.as_ref(), &fixture.dir, &["reference/y.md"], "add y");

    let found = failures(&fixture, &sha);

    assert_eq!(
        found,
        vec![SecretLikeFailure {
            path: "reference/y.md".to_string(),
            detail: format!("secret_like_content: reference/y.md (pem_block) @{}", &sha[..7]),
        }]
    );
    assert!(!found[0].detail.contains("MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSk"));
}

#[test]
fn given_a_vendor_token_in_the_description_when_scanned_then_the_full_content_is_refused() {
    let fixture = fixture(&[]);
    write_at(
        &fixture.dir,
        "reference/y.md",
        &format!("---\ndescription: {GHP}\n---\n\nclean body\n"),
    );
    let sha = commit_paths(fixture.exec.as_ref(), &fixture.dir, &["reference/y.md"], "add y");

    let found = failures(&fixture, &sha);

    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].detail,
        format!("secret_like_content: reference/y.md (vendor_token) @{}", &sha[..7])
    );
    assert!(!found[0].detail.contains(GHP));
}

#[test]
fn given_a_clean_changed_set_when_scanned_then_nothing_is_refused() {
    let fixture = fixture(&[]);
    write_at(&fixture.dir, "reference/ok.md", "---\ndescription: Ok\n---\n\nordinary note\n");
    let sha = commit_paths(fixture.exec.as_ref(), &fixture.dir, &["reference/ok.md"], "add ok");

    assert_eq!(failures(&fixture, &sha), Vec::new());
    assert_eq!(
        find_secret_like_failure(fixture.exec.as_ref(), &fixture.dir, &fixture.base, &sha)
            .expect("scan"),
        None
    );
}

#[test]
fn given_a_credential_only_in_an_earlier_commit_when_scanned_then_that_file_version_is_refused() {
    let fixture = fixture(&[]);
    write_at(&fixture.dir, "reference/z.md", "---\ndescription: Z\n---\n\ntoken=abc123456\n");
    let first = commit_paths(fixture.exec.as_ref(), &fixture.dir, &["reference/z.md"], "add z");
    write_at(&fixture.dir, "reference/z.md", "---\ndescription: Z\n---\n\nclean now\n");
    write_at(&fixture.dir, "reference/w.md", "---\ndescription: W\n---\n\nclean\n");
    let tip = commit_paths(
        fixture.exec.as_ref(),
        &fixture.dir,
        &["reference/z.md", "reference/w.md"],
        "clean up z and add w",
    );

    let found = failures(&fixture, &tip);

    assert_eq!(
        found,
        vec![SecretLikeFailure {
            path: "reference/z.md".to_string(),
            detail: format!(
                "secret_like_content: reference/z.md (credential_assignment) @{}",
                &first[..7]
            ),
        }]
    );
}

#[test]
fn given_a_non_markdown_blob_with_an_api_key_when_scanned_then_the_content_is_refused() {
    let fixture = fixture(&[]);
    write_at(
        &fixture.dir,
        "reference/data.json",
        "{\"api_key\": \"sk-proj-AAAABBBBCCCCDDDD\"}\n",
    );
    let sha = commit_paths(fixture.exec.as_ref(), &fixture.dir, &["reference/data.json"], "add data");

    let found = failures(&fixture, &sha);

    assert_eq!(found.len(), 1);
    assert!(found[0].detail.contains("(openai_key)"), "{}", found[0].detail);
    assert!(!found[0].detail.contains("sk-proj-AAAABBBBCCCCDDDD"));
}

#[test]
fn given_a_zero_width_evaded_credential_in_a_text_blob_when_scanned_then_it_is_refused() {
    let fixture = fixture(&[]);
    write_at(&fixture.dir, "reference/notes.txt", "x/token=\u{200b}abc123456\n");
    let sha = commit_paths(fixture.exec.as_ref(), &fixture.dir, &["reference/notes.txt"], "add notes");

    let found = failures(&fixture, &sha);

    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].detail,
        format!(
            "secret_like_content: reference/notes.txt (credential_assignment) @{}",
            &sha[..7]
        )
    );
    assert!(!found[0].detail.contains("abc123456"));
}

#[test]
fn given_a_non_utf8_blob_carrying_an_ascii_credential_when_scanned_then_the_bytes_still_match() {
    let fixture = fixture(&[]);
    let mut body = vec![0xffu8, 0xfe];
    body.extend_from_slice(b"x/token=abc123456\n");
    std::fs::create_dir_all(fixture.dir.join("reference")).expect("reference dir");
    std::fs::write(fixture.dir.join("reference/binary.bin"), &body).expect("write binary");
    let sha = commit_paths(fixture.exec.as_ref(), &fixture.dir, &["reference/binary.bin"], "add binary");

    let found = failures(&fixture, &sha);

    assert_eq!(found.len(), 1);
    assert!(
        found[0].detail.contains("(credential_assignment)"),
        "{}",
        found[0].detail
    );
}

#[test]
fn given_a_secret_like_file_name_with_a_clean_body_when_scanned_then_only_the_name_is_named() {
    let fixture = fixture(&[]);
    write_at(
        &fixture.dir,
        "reference/token=abc123456.md",
        "---\ndescription: T\n---\n\nclean body\n",
    );
    let sha = commit_paths(
        fixture.exec.as_ref(),
        &fixture.dir,
        &["reference/token=abc123456.md"],
        "add named file",
    );

    let found = failures(&fixture, &sha);

    assert_eq!(
        found,
        vec![SecretLikeFailure {
            path: "reference/token=abc123456.md".to_string(),
            detail: format!(
                "secret_like_content: reference/*** (credential_assignment, file name) @{}",
                &sha[..7]
            ),
        }]
    );
}

#[test]
fn given_a_secret_like_name_renamed_away_before_the_tip_when_scanned_then_the_introducing_version_is_refused() {
    let fixture = fixture(&[]);
    let bad_name = format!("reference/{XOXB}.md");
    write_at(&fixture.dir, &bad_name, "---\ndescription: T\n---\n\nclean body\n");
    let first = commit_paths(
        fixture.exec.as_ref(),
        &fixture.dir,
        &[bad_name.as_str()],
        "add badly named file",
    );
    git(
        fixture.exec.as_ref(),
        &fixture.dir,
        &["mv", bad_name.as_str(), "reference/clean.md"],
    );
    let tip = commit_paths(
        fixture.exec.as_ref(),
        &fixture.dir,
        &["reference/clean.md"],
        "rename to clean",
    );

    let found = failures(&fixture, &tip);

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, bad_name);
    assert!(found[0].detail.contains("(vendor_token, file name)"), "{}", found[0].detail);
    assert!(found[0].detail.ends_with(&first[..7]), "{}", found[0].detail);
    assert!(!found[0].detail.contains(XOXB));
}

#[test]
fn given_a_gitlink_when_scanned_then_it_is_refused_on_its_file_type_without_echoing_the_path() {
    let fixture = fixture(&[]);
    let head = fixture.base.clone();
    git(
        fixture.exec.as_ref(),
        &fixture.dir,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            "160000",
            head.as_str(),
            "reference/token=abc123456",
        ],
    );
    std::fs::create_dir_all(fixture.dir.join("reference/token=abc123456")).expect("submodule dir");
    git(
        fixture.exec.as_ref(),
        &fixture.dir,
        &[
            "-c",
            "user.email=fixture@example.com",
            "-c",
            "user.name=fixture",
            "commit",
            "-qm",
            "add gitlink",
        ],
    );
    let tip = git(fixture.exec.as_ref(), &fixture.dir, &["rev-parse", "HEAD"])
        .trim()
        .to_string();

    let found = failures(&fixture, &tip);

    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].detail.contains("(submodule, file type)"),
        "{}",
        found[0].detail
    );
    assert!(!found[0].detail.contains("abc123456"));
}

#[test]
fn given_a_type_change_to_a_symlink_targeting_a_credential_when_scanned_then_it_is_refused() {
    let fixture = fixture(&[("reference/link.txt", "clean\n")]);
    std::fs::remove_file(fixture.dir.join("reference/link.txt")).expect("remove file");
    #[cfg(unix)]
    std::os::unix::fs::symlink("x/token=\u{200b}abc123456", fixture.dir.join("reference/link.txt"))
        .expect("symlink");
    #[cfg(not(unix))]
    write_at(&fixture.dir, "reference/link.txt", "x/token=\u{200b}abc123456");
    let sha = commit_paths(
        fixture.exec.as_ref(),
        &fixture.dir,
        &["reference/link.txt"],
        "swap to symlink",
    );

    let found = failures(&fixture, &sha);

    assert_eq!(found.len(), 1, "{found:?}");
    assert!(
        found[0].detail.contains("(credential_assignment)"),
        "{}",
        found[0].detail
    );
    assert!(!found[0].detail.contains("abc123456"));
}

#[test]
fn given_a_merge_resolution_that_introduces_a_credential_when_scanned_then_the_merge_is_refused() {
    let fixture = fixture(&[("reference/m.md", "---\ndescription: M\n---\n\nbase\n")]);
    let exec = fixture.exec.as_ref();
    let branch = git(exec, &fixture.dir, &["symbolic-ref", "--short", "HEAD"])
        .trim()
        .to_string();
    git(exec, &fixture.dir, &["checkout", "-q", "-b", "memory/side-probe"]);
    write_at(&fixture.dir, "reference/m.md", "---\ndescription: M\n---\n\nside change\n");
    commit_paths(exec, &fixture.dir, &["reference/m.md"], "side change");
    git(exec, &fixture.dir, &["checkout", "-q", branch.as_str()]);
    write_at(&fixture.dir, "reference/m.md", "---\ndescription: M\n---\n\nmain change\n");
    commit_paths(exec, &fixture.dir, &["reference/m.md"], "main change");
    let _ = exec.run_in(&fixture.dir, &["merge", "--no-ff", "-m", "merge side", "memory/side-probe"]);
    write_at(
        &fixture.dir,
        "reference/m.md",
        "---\ndescription: M\n---\n\nresolved token=abc123456\n",
    );
    let merge_sha = commit_paths(exec, &fixture.dir, &["reference/m.md"], "merge side");
    write_at(&fixture.dir, "reference/m.md", "---\ndescription: M\n---\n\nresolved clean\n");
    let tip = commit_paths(exec, &fixture.dir, &["reference/m.md"], "clean resolution");

    let found = failures(&fixture, &tip);

    assert!(
        found.iter().any(|failure| failure.detail.ends_with(&merge_sha[..7])),
        "{found:?}"
    );
    assert!(
        found
            .iter()
            .all(|failure| !failure.detail.contains("abc123456")),
        "{found:?}"
    );
}
