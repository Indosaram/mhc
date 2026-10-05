use maho_agent::harness::session::{Entry, EntryKind, EntryScan, ScanOrder, UsageRow, UsageScan};
use maho_server::sqlite::{apply_initial_schema, entries::*};
use serde_json::json;
#[test]
fn entry_roundtrip_scan_filters_and_parent_integrity() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    apply_initial_schema(&db).unwrap();
    let first = Entry {
        id: "one".into(),
        parent_id: None,
        seq: 1,
        timestamp: 123,
        kind: EntryKind::Custom {
            custom_type: "test".into(),
            data: Some(json!(null)),
        },
    };
    let second = Entry {
        id: "two".into(),
        parent_id: Some("one".into()),
        seq: 2,
        timestamp: 124,
        kind: EntryKind::Custom {
            custom_type: "other".into(),
            data: None,
        },
    };
    assert!(insert_entry(&db, "s", &second).is_err());
    insert_entry(&db, "s", &first).unwrap();
    insert_entry(&db, "s", &second).unwrap();
    assert_eq!(
        scan_entries(
            &db,
            "s",
            &EntryScan {
                order: Some(ScanOrder::Desc),
                ..Default::default()
            }
        )
        .unwrap(),
        vec![second, first.clone()]
    );
    assert_eq!(
        scan_entries(
            &db,
            "s",
            &EntryScan {
                custom_type: Some("test".into()),
                from_seq: Some(1),
                to_seq: Some(1),
                ..Default::default()
            }
        )
        .unwrap(),
        vec![first]
    );
    assert!(
        scan_entries(&db, "other", &EntryScan::default())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn selected_entry_reads_do_not_decode_corrupt_unrequested_payloads() {
    let db = rusqlite::Connection::open_in_memory().unwrap();apply_initial_schema(&db).unwrap();
    let entry = Entry {id:"valid".into(),parent_id:None,seq:1,timestamp:0,kind:EntryKind::Custom {custom_type:"test".into(),data:None}};
    insert_entry(&db,"s",&entry).unwrap();
    let corrupt = Entry {id:"corrupt".into(),seq:2,..entry.clone()};insert_entry(&db,"s",&corrupt).unwrap();
    db.execute("UPDATE entries SET payload='invalid JSON' WHERE id='corrupt'",[]).unwrap();
    assert_eq!(read_entries(&db,"s",&["valid".into()]).unwrap(),vec![entry]);
    assert!(read_entries(&db,"s",&["corrupt".into()]).is_err());
}
#[test]
fn usage_roundtrip_preserves_adjustment_details_and_range() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    apply_initial_schema(&db).unwrap();
    let usage = UsageRow {
        id: "usage".into(),
        seq: 2,
        usage: Default::default(),
        entry_id: None,
        adjustment: true,
        details: Some(json!(null)),
    };
    insert_usage(&db, "s", &usage).unwrap();
    assert_eq!(
        scan_usage(&db, "s", &UsageScan::default()).unwrap(),
        vec![usage]
    );
    assert!(
        scan_usage(
            &db,
            "s",
            &UsageScan {
                from_seq: Some(3),
                ..Default::default()
            }
        )
        .unwrap()
        .is_empty()
    );
}
