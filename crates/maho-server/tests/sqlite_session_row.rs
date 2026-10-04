use maho_agent::harness::session::SessionMetadata;
use maho_server::sqlite::{apply_initial_schema, session_row::*};
#[test]
fn metadata_and_sequence_roundtrip_and_delete_are_session_scoped() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    apply_initial_schema(&db).unwrap();
    let metadata = SessionMetadata {
        id: "session".into(),
        created_at: 123,
        storage_version: 1,
        parent_session_id: Some("parent".into()),
        cwd: None,
        legacy_parent_session_path: None,
    };
    insert_session(&db, &metadata, 1).unwrap();
    assert!(has_session(&db, "session").unwrap());
    assert_eq!(
        metadata_from_row(read_session(&db, "session").unwrap(), 1).unwrap(),
        metadata
    );
    assert!(metadata_from_row(read_session(&db, "session").unwrap(), 0).is_err());
    assert!(metadata_from_row(read_session(&db, "session").unwrap(), 2).is_err());
    advance_next_seq(&db, "session", 4).unwrap();
    assert_eq!(read_session(&db, "session").unwrap().next_seq, 4);
    assert!(advance_next_seq(&db, "missing", 4).is_err());
    delete_session_rows(&db, "session").unwrap();
    assert!(!has_session(&db, "session").unwrap());
    assert!(delete_session_rows(&db, "session").is_err());
}
