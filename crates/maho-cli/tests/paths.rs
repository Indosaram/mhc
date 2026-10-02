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
