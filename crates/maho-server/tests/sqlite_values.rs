use maho_agent::harness::session::{ListCursor, ListOrder, ListReadOptions, Value, ValueList};
use maho_server::sqlite::{apply_initial_schema, values::*};
use serde_json::json;
#[test]
fn scalar_upsert_delete_and_unicode_prefix_are_session_scoped() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    apply_initial_schema(&db).unwrap();
    for key in [
        "a%",
        "a%1",
        "a_",
        "b",
        "\u{d7ff}",
        "\u{d7ff}x",
        "\u{e000}",
        "\u{10ffff}",
        "\u{10ffff}x",
    ] {
        let address = Value {
            namespace: "test".into(),
            key: key.into(),
        };
        set_scalar(&db, "s", &address, 1, &json!(key)).unwrap();
        set_scalar(&db, "other", &address, 2, &json!("other")).unwrap();
    }
    for (prefix, expected) in [("a%", 2), ("\u{d7ff}", 2), ("\u{10ffff}", 2), ("", 9)] {
        assert_eq!(
            scan_scalars(
                &db,
                "s",
                &Value {
                    namespace: "test".into(),
                    key: prefix.into()
                }
            )
            .unwrap()
            .len(),
            expected
        );
    }
    let address = Value {
        namespace: "test".into(),
        key: "a%".into(),
    };
    set_scalar(&db, "s", &address, 3, &json!(null)).unwrap();
    let stored = read_scalar(&db, "s", &address).unwrap().unwrap();
    assert_eq!(stored.seq, 3);
    assert_eq!(stored.value, json!(null));
    delete_scalar(&db, "s", &address).unwrap();
    assert!(read_scalar(&db, "s", &address).unwrap().is_none());
    assert!(read_scalar(&db, "other", &address).unwrap().is_some());
}
#[test]
fn list_reads_use_exclusive_cursors_in_both_directions() {
    let db = rusqlite::Connection::open_in_memory().unwrap();
    apply_initial_schema(&db).unwrap();
    let address = ValueList {
        namespace: "test".into(),
        key: "list".into(),
    };
    for seq in 1..=5 {
        append_list(&db, "s", &address, seq, &json!(seq)).unwrap();
    }
    for (order, expected) in [(ListOrder::Asc, vec![4, 5]), (ListOrder::Desc, vec![2, 1])] {
        let items = read_list(
            &db,
            "s",
            &address,
            Some(ListReadOptions {
                cursor: Some(ListCursor { seq: 3 }),
                order: Some(order),
                limit: Some(2),
            }),
        )
        .unwrap();
        assert_eq!(items.iter().map(|i| i.seq).collect::<Vec<_>>(), expected);
    }
    assert!(
        read_list(
            &db,
            "s",
            &address,
            Some(ListReadOptions {
                limit: Some(0),
                ..Default::default()
            })
        )
        .is_err()
    );
    assert!(append_list(&db, "s", &address, 1, &json!("duplicate")).is_err());
    delete_list(&db, "s", &address).unwrap();
    assert!(read_list(&db, "s", &address, None).unwrap().is_empty());
}
