use pretty_assertions::assert_eq;
use tempfile::tempdir;

use super::*;

#[test]
fn test_validate_memory_path_when_valid_tool_labels_given_then_resolves_to_confined_markdown_files()
{
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let cases = [
        ("system/notes", "system/notes.md"),
        ("system/notes.md", "system/notes.md"),
        ("memory/system/notes", "system/notes.md"),
        (
            "memory/reference/nested/topic.md",
            "reference/nested/topic.md",
        ),
        ("system//nested///notes", "system/nested/notes.md"),
        ("system\\nested\\notes.md", "system/nested/notes.md"),
        ("  system/notes.md  ", "system/notes.md"),
    ];

    for (input, expected) in cases {
        let result =
            validate_memory_path(&root, input, ValidateMemoryPathOptions::default()).unwrap();
        assert_eq!(result, root.join(expected));
    }
}

#[test]
fn test_validate_memory_path_when_absolute_path_inside_root_then_accepted() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let input = root.join("system").join("absolute.md");

    let result = validate_memory_path(
        &root,
        &input.to_string_lossy(),
        ValidateMemoryPathOptions::default(),
    )
    .unwrap();
    assert_eq!(result, input);
}

#[test]
fn test_validate_memory_path_when_hostile_labels_given_then_rejected() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let cases = [
        ("", "non-empty"),
        ("   ", "non-empty"),
        ("memory/", "empty"),
        ("../etc", "traversal"),
        ("a/../../etc", "traversal"),
        ("a/./notes", "traversal"),
        ("system/../notes", "traversal"),
        ("system/nu\0ll", "null"),
        ("~/x", "home-relative"),
        ("$HOME/x", "home-relative"),
        ("notes.txt", "markdown"),
        ("notes.MD", "markdown"),
        ("notes.Md", "markdown"),
        (".git/config", ".git"),
        (".GIT/config.md", ".git"),
        ("system/.Git/config.md", ".git"),
        ("memory/.gIt/config", ".git"),
    ];

    for (input, message) in cases {
        let err =
            validate_memory_path(&root, input, ValidateMemoryPathOptions::default()).unwrap_err();
        assert!(
            err.message.contains(message),
            "expected error containing '{message}', got '{err}' for input '{input}'"
        );
    }
}

#[test]
fn test_validate_memory_path_when_absolute_path_outside_root_then_rejected() {
    let base = tempdir().unwrap();
    let root = base.path().join("root");
    std::fs::create_dir(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let outside = base.path().join("outside.md");

    let err = validate_memory_path(
        &root,
        &outside.to_string_lossy(),
        ValidateMemoryPathOptions::default(),
    )
    .unwrap_err();
    assert!(err.message.contains("only be used"));
}

#[test]
fn test_validate_memory_path_when_root_itself_as_absolute_path_then_rejected() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let err = validate_memory_path(
        &root,
        &root.to_string_lossy(),
        ValidateMemoryPathOptions::default(),
    )
    .unwrap_err();
    assert!(err.message.contains("only be used"));
}

#[test]
fn test_validate_memory_path_when_windows_absolute_path_outside_root_then_rejected() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let err = validate_memory_path(
        &root,
        "C:\\Windows\\system.ini",
        ValidateMemoryPathOptions::default(),
    )
    .unwrap_err();
    assert!(err.message.contains("only be used"));
}

#[cfg(unix)]
#[test]
fn test_validate_memory_path_when_symlink_directory_escapes_root_then_rejected() {
    let base = tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let root = root.canonicalize().unwrap();

    std::os::unix::fs::symlink(&outside, root.join("escape")).unwrap();

    let err = validate_memory_path(
        &root,
        "escape/secret.md",
        ValidateMemoryPathOptions::default(),
    )
    .unwrap_err();
    assert!(err.message.contains("symlink"));
}

#[cfg(unix)]
#[test]
fn test_validate_memory_path_when_symlink_file_escapes_root_then_rejected() {
    let base = tempdir().unwrap();
    let root = base.path().join("root");
    let outside = base.path().join("outside.md");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(&outside, "secret").unwrap();
    let root = root.canonicalize().unwrap();

    std::os::unix::fs::symlink(&outside, root.join("linked.md")).unwrap();

    let err =
        validate_memory_path(&root, "linked.md", ValidateMemoryPathOptions::default()).unwrap_err();
    assert!(err.message.contains("symlink"));
}

#[test]
fn test_validate_memory_path_when_real_existing_parent_inside_root_then_missing_child_accepted() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    std::fs::create_dir(root.join("system")).unwrap();

    let result =
        validate_memory_path(&root, "system/new.md", ValidateMemoryPathOptions::default()).unwrap();
    assert_eq!(result, root.join("system").join("new.md"));
}

#[cfg(unix)]
#[test]
fn test_validate_memory_path_when_symlink_directory_inside_root_then_accepted() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();
    let system = root.join("system");
    std::fs::create_dir(&system).unwrap();
    std::os::unix::fs::symlink(&system, root.join("alias")).unwrap();

    let result = validate_memory_path(
        &root,
        "alias/notes.md",
        ValidateMemoryPathOptions::default(),
    )
    .unwrap();
    assert_eq!(result, root.join("alias").join("notes.md"));
}

#[test]
fn test_validate_repository_path_when_directory_label_given_then_no_extension_added() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let result = validate_repository_path(&root, "reference/archive").unwrap();
    assert_eq!(result, root.join("reference").join("archive"));
}

#[test]
fn test_validate_memory_path_when_non_markdown_file_and_tool_path_false_then_accepted() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let result = validate_memory_path(
        &root,
        "assets/tree.txt",
        ValidateMemoryPathOptions {
            tool_path: false,
            field_name: "path",
        },
    )
    .unwrap();
    assert_eq!(result, root.join("assets").join("tree.txt"));
}

#[test]
fn test_validate_repository_path_when_git_given_then_rejected() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let err = validate_repository_path(&root, ".git/config").unwrap_err();
    assert!(err.message.contains(".git"));
}

#[test]
fn test_validate_repository_path_when_traversal_given_then_rejected() {
    let tmp = tempdir().unwrap();
    let root = tmp.path().canonicalize().unwrap();

    let err = validate_repository_path(&root, "a/../../etc").unwrap_err();
    assert!(err.message.contains("traversal"));
}
