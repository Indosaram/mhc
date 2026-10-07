use super::*;

#[test]
fn exclusive_flag_detection_matches_the_pinned_predicate() {
    assert!(is_exclusive_flag("wx"));
    assert!(is_exclusive_flag("ax"));
    assert!(is_exclusive_flag("wX"));
    assert!(!is_exclusive_flag("w"));
    assert!(!is_exclusive_flag("a"));
}

#[test]
fn exclusive_open_is_not_retried_and_fails_when_the_path_exists() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("existing.txt");
    std::fs::write(&path, "seed").expect("seed");
    let error = open_with_exclusive_policy(&path, "wx").expect_err("exclusive open must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn write_path_all_writes_every_byte_and_appends_when_asked() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("note.txt");
    write_path_all(&path, b"first", "w", true).expect("write");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "first");
    write_path_all(&path, b"-second", "a", false).expect("append");
    assert_eq!(
        std::fs::read_to_string(&path).expect("read"),
        "first-second"
    );
}

#[test]
fn write_all_to_handle_reports_the_bytes_written() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("handle.txt");
    let mut handle = std::fs::File::create(&path).expect("create");
    assert_eq!(write_all_to_handle(&mut handle, b"abc").expect("write"), 3);
    drop(handle);
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "abc");
}
