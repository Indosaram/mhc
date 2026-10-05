use maho_server::app_server::history_pagination::*;
use serde_json::json;
fn options() -> HistoryPaginationOptions {
    HistoryPaginationOptions {
        kind: HistoryKind::Turn,
        thread_id: "t".into(),
        turn_id: None,
        limit: 2,
        sort_direction: SortDirection::Asc,
        cursor: None,
    }
}
fn values() -> Vec<HistoryValue<i32>> {
    (0..5)
        .map(|value| HistoryValue {
            key: value.to_string(),
            value,
        })
        .collect()
}
#[test]
fn forward_and_backwards_cursors_preserve_anchor_direction() {
    let mut options = options();
    let values = values();
    let first = paginate_history(&values, &options).unwrap();
    assert_eq!(first.data, [0, 1]);
    options.cursor = first.next_cursor;
    let second = paginate_history(&values, &options).unwrap();
    assert_eq!(second.data, [2, 3]);
    options.cursor = second.backwards_cursor.clone();
    assert_eq!(
        paginate_history(&values, &options).unwrap_err().code,
        -32600
    );
    options.sort_direction = SortDirection::Desc;
    let back = paginate_history(&values, &options).unwrap();
    assert_eq!(back.data, [2, 1]);
    options.cursor = second.next_cursor;
    assert_eq!(
        paginate_history(&values, &options).unwrap_err().code,
        -32600
    );
}
#[test]
fn inclusive_cursor_accepts_either_direction_and_scopes_to_thread() {
    let mut options = options();
    options.cursor = Some(inclusive_turn_history_cursor("t".into(), "2".into()).unwrap());
    assert_eq!(paginate_history(&values(), &options).unwrap().data, [2, 3]);
    options.sort_direction = SortDirection::Desc;
    assert_eq!(paginate_history(&values(), &options).unwrap().data, [2, 1]);
    options.thread_id = "other".into();
    assert_eq!(
        paginate_history(&values(), &options).unwrap_err().code,
        -32600
    );
}
#[test]
fn malformed_missing_or_stale_cursor_fails_and_empty_history_is_empty() {
    let mut options = options();
    assert_eq!(
        paginate_history::<i32>(&[], &options).unwrap(),
        HistoryPage {
            data: vec![],
            next_cursor: None,
            backwards_cursor: None
        }
    );
    for text in [
        "invalid".to_string(),
        json!({"kind":"turn","threadId":"t","anchor":"0","includeAnchor":true}).to_string(),
        inclusive_turn_history_cursor("t".into(), "missing".into()).unwrap(),
    ] {
        options.cursor = Some(text);
        assert_eq!(
            paginate_history(&values(), &options).unwrap_err().code,
            -32600
        );
    }
}
