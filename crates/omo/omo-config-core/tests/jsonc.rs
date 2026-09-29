use omo_config_core::internal::jsonc::{
    FormattingOptions, ModificationOptions, apply_edits, modify,
};
use omo_config_core::issue::PathSegment;
use omo_config_core::parse_jsonc_safe;
use serde_json::{Value, json};

fn options() -> ModificationOptions {
    ModificationOptions::formatted(FormattingOptions {
        eol: "\n".to_string(),
        insert_spaces: true,
        tab_size: 2,
        keep_lines: false,
        insert_final_newline: false,
    })
}

fn apply(text: &str, path: &[PathSegment], value: Option<Value>) -> String {
    let edits = modify(text, path, value.as_ref(), &options()).expect("modify");
    apply_edits(text, &edits).expect("apply")
}

fn key(value: &str) -> PathSegment {
    PathSegment::Key(value.to_string())
}

#[test]
fn given_commented_project_config_when_replacing_a_scalar_then_only_the_scalar_changes() {
    let text = "{\n  // task settings stay documented\n  \"task\": {\n    \"default_concurrency\": 5\n  }\n}\n";
    let edited = apply(
        text,
        &[key("task"), key("default_concurrency")],
        Some(json!(3)),
    );
    assert_eq!(
        edited,
        "{\n  // task settings stay documented\n  \"task\": {\n    \"default_concurrency\": 3\n  }\n}\n"
    );
}

#[test]
fn given_commented_project_config_when_adding_a_nested_key_then_the_new_object_is_formatted() {
    let text = "{\n  // task settings stay documented\n  \"task\": {\n    \"default_concurrency\": 3\n  }\n}\n";
    let edited = apply(
        text,
        &[key("task"), key("wait"), key("default_ms")],
        Some(json!(12000)),
    );
    assert_eq!(
        edited,
        "{\n  // task settings stay documented\n  \"task\": {\n    \"default_concurrency\": 3,\n    \"wait\": {\n      \"default_ms\": 12000\n    }\n  }\n}\n"
    );
}

#[test]
fn given_empty_document_when_inserting_a_nested_key_then_the_object_is_created() {
    let edited = apply(
        "// OMO configuration\n{\n}\n",
        &[key("task"), key("default_concurrency")],
        Some(json!(6)),
    );
    assert_eq!(
        edited,
        "// OMO configuration\n{\n  \"task\": {\n    \"default_concurrency\": 6\n  }\n}\n"
    );
}

#[test]
fn given_empty_document_when_inserting_an_object_then_the_object_is_created() {
    let edited = apply(
        "// OMO configuration\n{\n}\n",
        &[key("task")],
        Some(json!({ "wait": { "default_ms": 1 } })),
    );
    assert_eq!(
        edited,
        "// OMO configuration\n{\n  \"task\": {\n    \"wait\": {\n      \"default_ms\": 1\n    }\n  }\n}\n"
    );
}

#[test]
fn given_existing_array_when_replacing_then_the_reference_formatting_is_kept() {
    let edited = apply(
        "{\"_migrations\":[\"a\"]}\n",
        &[key("_migrations")],
        Some(json!(["a", "b"])),
    );
    assert_eq!(edited, "{\"_migrations\":[\n    \"a\",\n    \"b\"\n  ]}\n");
}

#[test]
fn given_empty_document_when_inserting_an_array_then_the_array_is_created() {
    let edited = apply(
        "// OMO configuration\n{\n}\n",
        &[key("_migrations")],
        Some(json!(["already-done"])),
    );
    assert_eq!(
        edited,
        "// OMO configuration\n{\n  \"_migrations\": [\n    \"already-done\"\n  ]\n}\n"
    );
}

#[test]
fn given_empty_object_when_inserting_an_object_then_the_object_is_created() {
    let edited = apply("{\n}\n", &[key("a")], Some(json!({ "b": 1 })));
    assert_eq!(edited, "{\n  \"a\": {\n    \"b\": 1\n  }\n}\n");
}

#[test]
fn given_commented_object_when_replacing_a_value_then_the_comment_survives() {
    let edited = apply(
        "{\n  // keep me\n  \"a\": 1\n}\n",
        &[key("a")],
        Some(json!(2)),
    );
    assert_eq!(edited, "{\n  // keep me\n  \"a\": 2\n}\n");
}

#[test]
fn given_two_keys_when_deleting_the_first_then_the_comma_is_removed() {
    let edited = apply("{\"a\":1,\"b\":2}\n", &[key("a")], None);
    assert_eq!(edited, "{\n  \"b\": 2\n}\n");
}

#[test]
fn given_two_keys_when_deleting_the_last_then_the_comma_is_removed() {
    let edited = apply("{\"a\":1,\"b\":2}\n", &[key("b")], None);
    assert_eq!(edited, "{\n  \"a\": 1\n}\n");
}

#[test]
fn given_single_key_when_deleting_it_then_only_the_empty_object_remains() {
    let edited = apply("{\n  \"a\": 1\n}\n", &[key("a")], None);
    assert_eq!(edited, "{\n}\n");
}

#[test]
fn given_missing_intermediate_objects_when_inserting_then_every_level_is_created() {
    let edited = apply("{\n}\n", &[key("x"), key("y"), key("z")], Some(json!(true)));
    assert_eq!(
        edited,
        "{\n  \"x\": {\n    \"y\": {\n      \"z\": true\n    }\n  }\n}\n"
    );
}

#[test]
fn given_existing_object_value_when_replacing_it_then_children_are_replaced() {
    let edited = apply(
        "{\n  \"a\": {\n    \"b\": 1\n  }\n}\n",
        &[key("a")],
        Some(json!({ "c": 2 })),
    );
    assert_eq!(edited, "{\n  \"a\": {\n    \"c\": 2\n  }\n}\n");
}

#[test]
fn given_tab_indented_object_when_inserting_then_the_inserted_property_is_reindented() {
    let edited = apply("{\n\t\"a\": 1\n}\n", &[key("b")], Some(json!(2)));
    assert_eq!(edited, "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
}

#[test]
fn given_trailing_comma_when_inserting_then_the_trailing_comma_stays() {
    let edited = apply("{\n  \"a\": 1,\n}\n", &[key("b")], Some(json!(2)));
    assert_eq!(edited, "{\n  \"a\": 1,\n  \"b\": 2,\n}\n");
}

#[test]
fn given_missing_property_when_deleting_then_no_edit_is_produced() {
    let edits = modify("{\"a\":1}\n", &[key("b")], None, &options()).expect("modify");
    assert!(edits.is_empty(), "expected no edits, got {edits:?}");
}

#[test]
fn given_invalid_document_when_deleting_at_the_root_then_the_reference_error_is_returned() {
    let result = modify("", &[key("a")], None, &options());
    assert!(result.is_err(), "expected an error for an empty document");
}

#[test]
fn given_jsonc_documents_when_parsed_safe_then_errors_and_values_match_the_reference() {
    let cases: Vec<(&str, bool)> = vec![
        ("", false),
        ("   \n", false),
        ("// comment\n", false),
        ("{\"a\":1,}", true),
        ("[1,2,]", true),
        ("{a:1}", false),
        ("{\"a\" 1}", false),
        ("[1 2]", false),
        ("{\"a\":}", false),
        ("tru", false),
        ("{\"a\":1} extra", false),
        ("01", false),
        ("+1", false),
        (".5", false),
        ("NaN", false),
        ("{\"a\":1e}", false),
        ("\"unterminated", false),
        ("{\"a\":\"\\q\"}", false),
        ("/* unterminated", false),
        ("{\"a\":\"\\u12\"}", false),
        ("{", false),
        ("[", false),
        ("[,]", false),
        ("{\"a\":}", false),
    ];
    for (input, valid) in cases {
        let result = parse_jsonc_safe(input);
        assert_eq!(
            result.errors.is_empty(),
            valid,
            "input {input:?} expected valid={valid} errors={:?}",
            result.errors
        );
    }
}

#[test]
fn given_bom_prefixed_document_when_parsed_safe_then_the_bom_is_stripped() {
    let result = parse_jsonc_safe("\u{feff}{\"a\":1}");
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.data, Some(json!({ "a": 1 })));
}

#[test]
fn given_null_document_when_parsed_safe_then_null_is_a_value_not_an_error() {
    let result = parse_jsonc_safe("null");
    assert!(result.errors.is_empty());
    assert_eq!(result.data, Some(Value::Null));
}

#[test]
fn given_duplicate_keys_when_parsed_safe_then_the_last_value_wins() {
    let result = parse_jsonc_safe("{\"a\":1,\"a\":2}");
    assert_eq!(result.data, Some(json!({ "a": 2 })));
}

#[test]
fn given_escaped_strings_when_parsed_safe_then_the_value_is_decoded() {
    let result = parse_jsonc_safe("{\"a\":\"x\\ny\\u0041\"}");
    assert_eq!(result.data, Some(json!({ "a": "x\nyA" })));
}

#[test]
fn given_array_document_when_deleting_the_only_item_then_the_array_is_emptied() {
    let edits = modify(
        "{\n  \"a\": [1]\n}\n",
        &[key("a"), PathSegment::Index(0)],
        None,
        &options(),
    )
    .expect("modify");
    let edited = apply_edits("{\n  \"a\": [1]\n}\n", &edits).expect("apply");
    assert_eq!(edited, "{\n  \"a\": []\n}\n");
}

#[test]
fn given_array_document_when_appending_with_index_minus_one_then_the_item_is_appended() {
    let edits = modify(
        "{\n  \"a\": [1]\n}\n",
        &[key("a"), PathSegment::Index(-1)],
        Some(&json!(2)),
        &options(),
    )
    .expect("modify");
    let edited = apply_edits("{\n  \"a\": [1]\n}\n", &edits).expect("apply");
    assert!(edited.contains("1"));
    assert!(edited.contains("2"));
}
