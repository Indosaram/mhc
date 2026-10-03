use maho_codemode::output::output_meta::*;

#[test]
fn elision_marker_uses_bytes_or_lines() {
    assert!(format_middle_elision_marker(1, 12).contains("12B"));
    assert!(format_middle_elision_marker(3, 12).contains("3ln"));
}

#[test]
fn artifacts_follow_session_file() {
    let root = tempfile::tempdir().unwrap();
    let session = root.path().join("session.jsonl");
    let artifacts = resolve_session_artifacts_dir(Some(&session)).unwrap();
    assert_eq!(artifacts.dir, root.path().join("session-artifacts"));
    assert!(!artifacts.temp);
    assert!(artifacts.dir.is_dir());
}

#[test]
fn temporary_artifact_directory_is_marked() {
    let artifacts = resolve_session_artifacts_dir(None).unwrap();
    assert!(artifacts.temp);
    assert!(artifacts.dir.is_dir());
    std::fs::remove_dir(&artifacts.dir).unwrap();
}

#[test]
fn notice_strip_only_removes_generated_suffix() {
    let meta = TruncationMeta { direction: Direction::Tail, truncated_by: TruncatedBy::Bytes,
        total_lines: 10, total_bytes: 100, output_lines: 3, output_bytes: 5,
        max_bytes: Some(5), max_columns: None, column_truncated_lines: None,
        shown_range: Some(LineRange { start: 8, end: 10 }), head_range: None, tail_range: None,
        elided_bytes: None, elided_lines: None, artifact_id: Some("/tmp/full.log".into()),
    };
    let notice = format_truncation_warning(Some(&meta)).unwrap();
    let output = format!("visible\n\n{notice}\n");
    assert_eq!(strip_output_notice(&output, Some(&meta)), "visible");
    assert_eq!(strip_output_notice("other\n[note]", Some(&meta)), "other\n[note]");
    assert!(format_truncation_warning(None).is_none());
}
