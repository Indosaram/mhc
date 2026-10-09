use std::fs;
use std::path::Path;

use comment_checker_core::*;
use tempfile::tempdir;

#[test]
fn given_a_cache_slot_without_a_marker_when_checked_then_it_is_not_current() {
    let temp = tempdir().expect("tempdir");
    let current = is_cached_comment_checker_current(temp.path(), None);
    assert!(!current);
}

#[test]
fn given_a_recorded_release_when_checked_then_it_is_current_and_the_marker_holds_the_pinned_version()
{
    let temp = tempdir().expect("tempdir");
    record_cached_comment_checker_release(temp.path()).expect("record release");
    let current = is_cached_comment_checker_current(temp.path(), None);
    assert!(current);
    let marker_content = fs::read_to_string(temp.path().join(COMMENT_CHECKER_VERSION_MARKER))
        .expect("read version marker");
    assert_eq!(
        marker_content,
        format!("{COMMENT_CHECKER_RELEASE_VERSION}
")
    );
}

#[test]
fn given_a_marker_from_another_release_when_checked_then_it_is_not_current() {
    let custom_read = |_path: &Path| Ok("0.6.1
".to_string());
    let current = is_cached_comment_checker_current(
        Path::new("/unused"),
        Some(&custom_read),
    );
    assert!(!current);
}
