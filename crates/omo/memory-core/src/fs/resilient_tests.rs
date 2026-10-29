use super::*;

#[test]
fn write_then_read_round_trips_and_append_extends() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("memo.txt");
    write(&path, b"alpha").expect("write");
    assert_eq!(read_to_string(&path).expect("read"), "alpha");
    append(&path, b"+beta").expect("append");
    assert_eq!(read(&path).expect("read bytes"), b"alpha+beta");
    assert!(exists(&path));
    assert!(!exists(&dir.path().join("missing.txt")));
}

#[test]
fn create_exclusive_refuses_a_second_creation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("once.txt");
    let handle = create_exclusive(&path).expect("first create");
    close_sync(handle);
    let error = create_exclusive(&path).expect_err("second create must fail");
    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
}

#[test]
fn directory_listing_is_sorted_and_tolerates_a_missing_directory() {
    let dir = tempfile::tempdir().expect("tempdir");
    create_dir_all(&dir.path().join("b")).expect("mkdir b");
    create_dir_all(&dir.path().join("a")).expect("mkdir a");
    std::fs::write(dir.path().join("file.txt"), "x").expect("file");
    assert_eq!(
        read_dir_names(dir.path()).expect("names"),
        vec!["a".to_string(), "b".to_string(), "file.txt".to_string()]
    );
    assert_eq!(
        read_dir_directories(dir.path()).expect("dirs"),
        vec!["a".to_string(), "b".to_string()]
    );
    assert_eq!(
        read_dir_directories(&dir.path().join("absent")).expect("missing dir"),
        Vec::<String>::new()
    );
}

#[test]
fn rename_moves_a_file_and_remove_file_deletes_it() {
    let dir = tempfile::tempdir().expect("tempdir");
    let from = dir.path().join("from.txt");
    let to = dir.path().join("to.txt");
    write(&from, b"payload").expect("write");
    rename(&from, &to).expect("rename");
    assert!(!exists(&from));
    assert_eq!(read_to_string(&to).expect("read"), "payload");
    remove_file(&to).expect("remove");
    assert!(!exists(&to));
}
