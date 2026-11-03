use maho_cli::utils::paths::*;
#[test]
fn normalization_can_disable_tilde_and_fold_all_unicode_spaces() {
    let options = PathInputOptions { expand_tilde: false, normalize_unicode_spaces: true, ..Default::default() };
    assert_eq!(normalize_path("~/a\u{2008}b\u{205f}c\u{3000}d", &options).unwrap(), "~/a b c d");
    assert_eq!(normalize_path("file:///tmp/a%20b", &options).unwrap(), "/tmp/a b");
    assert!(normalize_path("file://remote/path", &options).is_err());
}
#[cfg(unix)]
#[test]
fn revision_contains_real_filesystem_identity() {
    use std::os::unix::fs::MetadataExt;
    let file = tempfile::NamedTempFile::new().unwrap();
    let metadata = file.as_file().metadata().unwrap();
    assert!(get_file_revision(file.path().to_str().unwrap()).unwrap().starts_with(&format!("{}:{}:0:", metadata.dev(), metadata.ino())));
}

// senpi paths.test.ts
mod upstream {
    use maho_cli::utils::paths::*;
    use std::path::{Path, PathBuf};

    fn real(path: &Path) -> String {
        // senpi `realpathSync` never answers in the Windows verbatim (`\\?\`) form.
        text(&strip_verbatim_prefix(std::fs::canonicalize(path).expect("real path")))
    }
    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }
    fn file_url(path: &Path) -> String {
        url::Url::from_file_path(path).expect("absolute path").to_string()
    }
    fn cwd() -> PathBuf {
        std::env::temp_dir().join("pi-paths-cwd")
    }

    #[test]
    fn canonicalize_returns_the_real_path_for_a_regular_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "hello").unwrap();
        assert_eq!(canonicalize_path(&text(&file)), real(&file));
    }
    #[cfg(unix)]
    #[test]
    fn canonicalize_resolves_file_and_directory_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, "hello").unwrap();
        std::os::unix::fs::symlink(&target, dir.path().join("link.txt")).unwrap();
        assert_eq!(canonicalize_path(&text(&dir.path().join("link.txt"))), real(&target));
        std::fs::create_dir(dir.path().join("target-dir")).unwrap();
        std::os::unix::fs::symlink(dir.path().join("target-dir"), dir.path().join("link-dir")).unwrap();
        assert_eq!(canonicalize_path(&text(&dir.path().join("link-dir"))), real(&dir.path().join("target-dir")));
    }
    #[cfg(unix)]
    #[test]
    fn canonicalize_falls_back_to_the_raw_path_for_missing_and_dangling_targets() {
        let dir = tempfile::tempdir().unwrap();
        let missing = text(&dir.path().join("no-such-file"));
        assert_eq!(canonicalize_path(&missing), missing);
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(dir.path().join("target.txt"), &link).unwrap();
        assert_eq!(canonicalize_path(&text(&link)), text(&link));
    }
    #[test]
    fn cwd_relative_paths_keep_dot_names_and_reject_traversals() {
        let base = cwd();
        let inside = text(&base.join("..config").join("AGENTS.md"));
        assert_eq!(get_cwd_relative_path(&inside, &text(&base)).unwrap(), Some(text(&Path::new("..config").join("AGENTS.md"))));
        let outside = text(&base.join("..").join("AGENTS.md"));
        assert_eq!(get_cwd_relative_path(&outside, &text(&base)).unwrap(), None);
    }
    #[test]
    fn only_home_tilde_shortcuts_expand() {
        let home = maho_core::config::home_dir();
        let options = PathInputOptions::default();
        assert_eq!(normalize_path("~", &options).unwrap(), home);
        assert_eq!(normalize_path("~/file.txt", &options).unwrap(), text(&Path::new(&home).join("file.txt")));
        assert_eq!(resolve_path("~draft.md", &text(&cwd()), &options).unwrap(), text(&cwd().join("~draft.md")));
        assert_eq!(normalize_path("~draft.md", &options).unwrap(), "~draft.md");
    }
    #[test]
    fn relative_paths_resolve_against_a_path_or_file_url_base() {
        let options = PathInputOptions::default();
        let expected = text(&cwd().join("subdir").join("file.txt"));
        assert_eq!(resolve_path("subdir/file.txt", &text(&cwd()), &options).unwrap(), expected);
        assert_eq!(resolve_path("subdir/file.txt", &file_url(&cwd()), &options).unwrap(), expected);
    }
    #[test]
    fn file_urls_are_accepted_and_malformed_ones_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("file with spaces.txt");
        let options = PathInputOptions::default();
        assert_eq!(resolve_path(&file_url(&file), &text(&dir.path().join("base")), &options).unwrap(), text(&file));
        assert!(resolve_path("file:///%E0%A4%A", &text(&cwd()), &options).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn posix_absolute_paths_keep_literal_percent_sequences() {
        let dir = tempfile::tempdir().unwrap();
        let options = PathInputOptions::default();
        for name in ["report%2026.md", "foo%2Fbar", "malformed%A.md"] {
            let path = text(&dir.path().join(name));
            assert_eq!(resolve_path(&path, &text(&dir.path().join("base")), &options).unwrap(), path);
        }
    }
    #[test]
    fn windows_shell_drive_paths_convert_and_others_stay() {
        assert_eq!(normalize_windows_shell_path("/c/Users/example/project"), "C:\\Users\\example\\project");
        assert_eq!(normalize_windows_shell_path("/cygdrive/d/work"), "D:\\work");
        assert_eq!(normalize_windows_shell_path("/mnt/e/source"), "E:\\source");
        assert_eq!(normalize_windows_shell_path("/c"), "C:\\");
        for path in ["C:/Users/example", "C:\\Users\\example", "//server/share/file", "/c/Users\\example", "relative/file", "/tmp/file"] {
            assert_eq!(normalize_windows_shell_path(path), path);
        }
    }
    #[test]
    fn local_paths_exclude_package_and_remote_protocols() {
        for local in ["my-package", "./foo", "file:///tmp/foo"] {
            assert!(is_local_path(local), "{local}");
        }
        for remote in ["npm:package", "git://repo", "https://example.com"] {
            assert!(!is_local_path(remote), "{remote}");
        }
    }
    #[test]
    fn the_strict_form_refuses_what_the_convenience_form_returns_raw() {
        let dir = tempfile::tempdir().unwrap();
        let missing = text(&dir.path().join("does-not-exist").join("child"));
        assert_eq!(canonicalize_path(&missing), missing);
        assert_eq!(canonicalize_path_strict(&missing).unwrap_err().kind(), std::io::ErrorKind::NotFound);
        let file = dir.path().join("file.txt");
        std::fs::write(&file, "x").unwrap();
        assert_eq!(canonicalize_path_strict(&text(&file)).unwrap(), canonicalize_path(&text(&file)));
    }
}
