use super::*;

fn item(key: &str, ops_len: usize) -> WireItem {
    WireItem {
        key: key.into(),
        ops: "A".repeat(ops_len),
    }
}

#[test]
fn splits_in_capture_order_at_the_limit() -> Result<(), OversizedItemError> {
    // 8 base64 chars = 6 plaintext bytes; limit 10 fits one per batch, 4 chars (3 bytes) pack.
    let items = [item("a", 8), item("b", 4), item("c", 4), item("d", 8)];
    let batches = split_batches(&items, 10)?;
    let keys: Vec<Vec<String>> = batches
        .iter()
        .map(|b| b.iter().map(|i| i.key.clone()).collect())
        .collect();
    let expected: Vec<Vec<String>> =
        vec![vec!["a".into(), "b".into()], vec!["c".into(), "d".into()]];
    assert_eq!(keys, expected);
    Ok(())
}

#[test]
fn an_item_over_the_bound_is_refused_with_its_size() {
    let error = split_batches(&[item("big", 40)], 10).err();
    assert_eq!(
        error.map(|e| e.to_string()),
        Some("item big (30 bytes) exceeds the 10-byte batch bound".to_string())
    );
}

#[test]
fn no_items_make_no_batches() -> Result<(), OversizedItemError> {
    assert!(split_batches(&[], MAX_BATCH_BYTES)?.is_empty());
    Ok(())
}
