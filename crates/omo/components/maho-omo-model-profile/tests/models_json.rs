use std::sync::Arc;

use maho_omo_model_profile::models_json::read_models_json;
use serde_json::json;

#[test]
fn an_unchanged_models_json_is_parsed_once_and_the_same_parse_comes_back() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("models.json"),
        r#"{ "providers": { "proxy": { "models": [{ "id": "a" }] } } }"#,
    )
    .expect("models.json");

    let first = read_models_json(dir.path()).expect("a parse");
    assert_eq!(*first, json!({"providers": {"proxy": {"models": [{"id": "a"}]}}}));

    let second = read_models_json(dir.path()).expect("a parse");
    assert!(Arc::ptr_eq(&first, &second), "the cached parse is handed back");
}

#[test]
fn an_edited_or_removed_models_json_is_reread_or_reported_missing() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = dir.path().join("models.json");
    std::fs::write(&path, r#"{ "providers": {} }"#).expect("models.json");
    assert_eq!(*read_models_json(dir.path()).expect("a parse"), json!({"providers": {}}));

    std::fs::write(&path, r#"{ "providers": { "proxy": { "models": [{ "id": "a" }] } } }"#)
        .expect("models.json");
    assert_eq!(
        *read_models_json(dir.path()).expect("a parse"),
        json!({"providers": {"proxy": {"models": [{"id": "a"}]}}})
    );

    std::fs::remove_file(&path).expect("remove models.json");
    assert!(read_models_json(dir.path()).is_none(), "a missing file evicts the cached parse");
}

#[test]
fn jsonc_comments_parse_exactly_as_the_shared_reader_parses_them() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(
        dir.path().join("models.json"),
        "{\n  // JSONC, as senpi parses it\n  \"providers\": { \"proxy\": { \"models\": [] } },\n}\n",
    )
    .expect("models.json");
    assert_eq!(
        *read_models_json(dir.path()).expect("a parse"),
        json!({"providers": {"proxy": {"models": []}}})
    );
}
