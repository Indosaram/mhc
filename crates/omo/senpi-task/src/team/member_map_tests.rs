//! `team/member-map.test.ts`

use pretty_assertions::assert_eq;

use crate::team::member_map::{MemberTaskMap, member_task_map_path, read_member_task_map, write_member_task_map};

#[test]
fn given_a_written_map_when_read_back_then_the_member_task_mapping_round_trips() {
    // given
    let dir = tempfile::tempdir().expect("tempdir");
    let mut written = MemberTaskMap::new();
    written.insert("alpha".to_string(), "st_000001".to_string());
    written.insert("beta".to_string(), "st_000002".to_string());
    write_member_task_map(dir.path(), &written).expect("write");

    // when
    let map = read_member_task_map(dir.path());

    // then
    assert_eq!(map, written);
    assert!(member_task_map_path(dir.path()).exists());
}

#[test]
fn given_no_sidecar_file_when_read_then_an_empty_map_is_returned() {
    let dir = tempfile::tempdir().expect("tempdir");

    let map = read_member_task_map(dir.path());

    assert_eq!(map, MemberTaskMap::new());
}

#[test]
fn given_a_malformed_sidecar_when_read_then_an_empty_map_is_returned() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(member_task_map_path(dir.path()), "{ not json").expect("write");

    let map = read_member_task_map(dir.path());

    assert_eq!(map, MemberTaskMap::new());
}

#[test]
fn given_a_completed_write_when_the_directory_is_listed_then_no_temp_file_remains() {
    // given
    let dir = tempfile::tempdir().expect("tempdir");
    let mut written = MemberTaskMap::new();
    written.insert("alpha".to_string(), "st_000001".to_string());
    write_member_task_map(dir.path(), &written).expect("write");

    // when
    let entries: Vec<String> = std::fs::read_dir(dir.path())
        .expect("read_dir")
        .map(|entry| entry.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();

    // then
    assert_eq!(entries, vec!["senpi-task-members.json".to_string()]);
}
