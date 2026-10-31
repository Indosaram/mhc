use std::path::Path;

use tempfile::tempdir;
use utils::{PathContainmentOptions, contained_relative_path, is_path_inside};

#[test]
fn canonical_containment_accepts_parent_and_descendants() {
    let root = tempdir().unwrap();
    let child = root.path().join("child.txt");
    std::fs::write(&child, "x").unwrap();
    assert_eq!(
        contained_relative_path(&child, root.path(), PathContainmentOptions::default())
            .unwrap()
            .unwrap(),
        Path::new("child.txt")
    );
    assert_eq!(
        contained_relative_path(root.path(), root.path(), PathContainmentOptions::default())
            .unwrap()
            .unwrap(),
        Path::new("")
    );
}

#[test]
fn canonical_containment_rejects_outside_and_propagates_missing() {
    let root = tempdir().unwrap();
    let outside = tempdir().unwrap();
    assert!(
        !is_path_inside(
            outside.path(),
            root.path(),
            PathContainmentOptions::default()
        )
        .unwrap()
    );
    let missing = root.path().join("missing");
    assert!(is_path_inside(missing, root.path(), PathContainmentOptions::default()).is_err());
}

#[test]
fn containment_accepts_a_plain_dot_dot_path_without_links() {
    let root = tempdir().unwrap();
    let inside = root.path().join("inside");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    let lexical = inside.join("..").join("inside");
    assert!(is_path_inside(lexical, root.path(), PathContainmentOptions::default()).unwrap());
}

#[test]
fn windows_normalized_long_names_and_case() {
    let realpath = |path: &Path| -> std::io::Result<std::path::PathBuf> {
        let path = path
            .to_string_lossy()
            .to_lowercase()
            .replace("runner~1", "runneradmin");
        if path.ends_with("missing.md") {
            return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        }
        Ok(path.into())
    };
    let options = PathContainmentOptions {
        platform: Some("win32"),
        realpath: Some(&realpath),
    };
    let parent = r"C:\Users\runneradmin\AppData\Local\Temp";
    for (child, expected) in [
        (r"C:\Users\RUNNER~1\AppData\Local\Temp\note.md", true),
        (r"c:/USERS/RUNNER~1/AppData/Local/Temp\note.md", true),
        (r"C:\Users\RUNNER~1\AppData\Local\Temp", true),
        (r"C:\Users\RUNNER~1\AppData\Local\Temp-other\note.md", false),
        (
            r"C:\Users\RUNNER~1\AppData\Local\Temp\..\private\note.md",
            false,
        ),
        (r"D:\Users\runneradmin\AppData\Local\Temp\note.md", false),
    ] {
        assert_eq!(is_path_inside(child, parent, options).unwrap(), expected);
    }
    assert!(
        is_path_inside(
            r"C:\Users\RUNNER~1\AppData\Local\Temp\missing.md",
            parent,
            options
        )
        .is_err()
    );
    assert!(
        is_path_inside(
            parent.to_owned() + r"\note.md",
            r"C:\Users\RUNNER~1\AppData\Local\Temp",
            options
        )
        .unwrap()
    );
}

#[test]
fn posix_case_is_preserved() {
    let realpath = |path: &Path| Ok(path.to_path_buf());
    assert!(
        !is_path_inside(
            "/memory/PROJECT/notes.md",
            "/memory/project",
            PathContainmentOptions {
                platform: Some("linux"),
                realpath: Some(&realpath)
            }
        )
        .unwrap()
    );
}

#[cfg(unix)]
#[test]
fn native_symlink_dot_dot_and_lexical_input_have_distinct_contracts() {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let inside = root.path().join("inside");
    let outside = root.path().join("outside");
    std::fs::create_dir_all(inside.join("workspace")).unwrap();
    std::fs::create_dir_all(outside.join("subdir")).unwrap();
    std::fs::create_dir_all(outside.join("workspace")).unwrap();
    symlink(outside.join("subdir"), inside.join("jump")).unwrap();
    symlink("jump/..", inside.join("entry")).unwrap();
    assert!(
        !is_path_inside(
            inside.join("entry/workspace"),
            &inside,
            PathContainmentOptions::default()
        )
        .unwrap()
    );
    // dot-dot after a link in the input: the verdict is about the path the OS opens, which
    // follows `jump` before applying `..` (outside/workspace).
    assert!(
        !is_path_inside(
            format!("{}/jump/../workspace", inside.display()),
            &inside,
            PathContainmentOptions::default()
        )
        .unwrap()
    );
}

#[cfg(unix)]
fn symlink_fixture() -> (tempfile::TempDir, std::path::PathBuf) {
    use std::os::unix::fs::symlink;
    let root = tempdir().unwrap();
    let base = std::fs::canonicalize(root.path()).unwrap();
    let inside = base.join("inside");
    let outside = base.join("outside");
    std::fs::create_dir_all(inside.join("workspace")).unwrap();
    std::fs::create_dir_all(outside.join("subdir")).unwrap();
    std::fs::create_dir_all(outside.join("workspace")).unwrap();
    std::fs::write(inside.join("workspace/note.md"), "inside").unwrap();
    std::fs::write(outside.join("workspace/note.md"), "outside").unwrap();
    symlink(outside.join("subdir"), inside.join("jump")).unwrap();
    symlink("jump/..", inside.join("entry")).unwrap();
    (root, inside)
}

#[cfg(unix)]
#[test]
fn existing_inside_and_different_directories_stay_distinct() {
    let (_root, inside) = symlink_fixture();
    let options = PathContainmentOptions::default();
    assert_eq!(
        contained_relative_path(inside.join("workspace").join("note.md"), &inside, options)
            .unwrap(),
        Some(Path::new("workspace").join("note.md"))
    );
    assert_eq!(
        contained_relative_path(inside.join("..").join("outside"), &inside, options).unwrap(),
        None
    );
    assert_eq!(
        contained_relative_path(&inside, &inside, options).unwrap(),
        Some(std::path::PathBuf::new())
    );
}

#[cfg(unix)]
#[test]
fn dot_dot_after_a_link_in_the_parent_is_also_resolved_natively() {
    let (_root, inside) = symlink_fixture();
    let parent = format!("{}/jump/..", inside.display());
    assert!(
        !is_path_inside(
            inside.join("workspace"),
            parent,
            PathContainmentOptions::default()
        )
        .unwrap()
    );
}

#[cfg(target_os = "macos")]
#[test]
fn a_case_alias_of_the_same_directory_is_accepted_by_native_containment() {
    let (_root, inside) = symlink_fixture();
    let alias = inside.with_file_name("INSIDE");
    // Case-sensitive macOS volumes have distinct directory names, not an alias.
    if !alias.exists() {
        assert!(
            contained_relative_path(&alias, &inside, PathContainmentOptions::default()).is_err()
        );
        return;
    }
    assert_eq!(
        contained_relative_path(&alias, &inside, PathContainmentOptions::default()).unwrap(),
        Some(std::path::PathBuf::new())
    );
}
