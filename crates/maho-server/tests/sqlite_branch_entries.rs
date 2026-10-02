use maho_agent::harness::session::{Entry, EntryKind};
use maho_server::sqlite::{
    apply_initial_schema, branch_entries::append_to_branch_index, entries::insert_entry,
};
#[test]
fn root_tip_and_divergent_branch_index_preserves_both_paths() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    apply_initial_schema(&db).unwrap();
    for (id, parent, seq) in [
        ("root", None, 1),
        ("left", Some("root"), 2),
        ("right", Some("root"), 3),
    ] {
        let entry = Entry {
            id: id.into(),
            parent_id: parent.map(str::to_owned),
            seq,
            timestamp: 0,
            kind: EntryKind::Custom {
                custom_type: "test".into(),
                data: None,
            },
        };
        insert_entry(&db, "s", &entry).unwrap();
        append_to_branch_index(&db, "s", &entry).unwrap();
    }
    let branches = db
        .prepare("SELECT branch_id,tip_entry_id FROM branch_meta ORDER BY branch_id")
        .unwrap()
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        branches,
        vec![
            ("right".into(), "right".into()),
            ("root".into(), "left".into())
        ]
    );
    let entries = db
        .prepare("SELECT entry_id FROM branch_entries WHERE branch_id='right' ORDER BY entry_seq")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(entries, vec!["root", "right"]);
    let mut query = maho_agent::harness::session::StorageBranchScan::new("right");
    assert_eq!(
        maho_server::sqlite::branch_entries::scan_branch(&db, "s", &query)
            .unwrap()
            .iter()
            .map(|e| e.id.as_str())
            .collect::<Vec<_>>(),
        vec!["right", "root"]
    );
    query.order = Some(maho_agent::harness::session::BranchOrder::OldestFirst);
    query.stop_at_id = Some("root".into());
    assert_eq!(
        maho_server::sqlite::branch_entries::scan_branch(&db, "s", &query)
            .unwrap()
            .len(),
        1
    );
    query.stop_at_id = None;
    query.cursor = Some(maho_agent::harness::session::EntryCursor { seq: 1 });
    assert_eq!(
        maho_server::sqlite::branch_entries::scan_branch(&db, "s", &query).unwrap()[0].id,
        "right"
    );
    db.execute("UPDATE entries SET payload='invalid JSON' WHERE session_id='s' AND id='right'",[]).unwrap();
    assert!(maho_server::sqlite::branch_entries::scan_branch(&db,"s",&query).is_err());
    let structures = maho_server::sqlite::branch_entries::scan_branch_structure(&db,"s",&query).unwrap();
    assert_eq!(structures[0].id,"right");
    assert_eq!(structures[0].custom_type.as_deref(),Some("test"));
}
