use std::{path::Path, sync::Arc};
use maho_tools::{definition::*, edit::{create_edit_tool_definition, prepare_edit_arguments, EditToolOptions}, read_classifiers::*};
use serde_json::json;

#[test]
fn edit_schema_has_no_legacy_fields() {
    let definition = create_edit_tool_definition(".".into(), EditToolOptions::default());
    assert!(definition.parameters["properties"].get("oldText").is_none());
    assert!(definition.parameters["properties"].get("newText").is_none());
}

#[test]
fn edit_folds_legacy_fields() {
    assert_eq!(prepare_edit_arguments(json!({"path":"file", "oldText":"a", "newText":"b"})).unwrap(),
        json!({"path":"file", "edits":[{"oldText":"a", "newText":"b"}]}));
}

#[test]
fn edit_appends_legacy_to_existing_edits() {
    assert_eq!(prepare_edit_arguments(json!({"path":"file", "edits":[{"oldText":"a", "newText":"b"}], "oldText":"c", "newText":"d"})).unwrap(),
        json!({"path":"file", "edits":[{"oldText":"a", "newText":"b"}, {"oldText":"c", "newText":"d"}]}));
}

#[test]
fn edit_preserves_valid_input() {
    let input = json!({"path":"file", "edits":[{"oldText":"a", "newText":"b"}]});
    assert_eq!(prepare_edit_arguments(input.clone()).unwrap(), input);
}

#[test]
fn edit_preserves_nonobject_input() {
    for input in [json!(null), json!("garbage"), json!(1)] {
        assert_eq!(prepare_edit_arguments(input.clone()).unwrap(), input);
    }
}

#[test]
fn edit_parses_stringified_edits() {
    assert_eq!(prepare_edit_arguments(json!({"edits":"[{\"oldText\":\"a\",\"newText\":\"b\"}]"})).unwrap(),
        json!({"edits":[{"oldText":"a", "newText":"b"}]}));
}

#[test]
fn edit_preserves_invalid_json_string() {
    let input = json!({"edits":"not json"});
    assert_eq!(prepare_edit_arguments(input.clone()).unwrap(), input);
}

#[test]
fn classifiers_decline_claim_skip_errors_and_unregister() {
    let path = Path::new("/unique-contract-path");
    let cwd = Path::new("/project");
    assert!(classify_read(path, cwd).is_none());
    let declining = register_read_classifier(Arc::new(|_, _| Ok(None))).unwrap();
    let failing = register_read_classifier(Arc::new(|_, _| Err("failed".into()))).unwrap();
    let claiming = register_read_classifier(Arc::new(move |received_path, received_cwd| {
        assert_eq!(received_path, path);
        assert_eq!(received_cwd, cwd);
        Ok(Some(CompactReadClassification { kind: CompactReadKind::Memory, label: "preference".into(), headline: None }))
    })).unwrap();
    let later = register_read_classifier(Arc::new(|_, _| panic!("first match must win"))).unwrap();
    assert_eq!(classify_read(path, cwd).unwrap().kind, CompactReadKind::Memory);
    drop(later);
    drop(claiming);
    assert!(classify_read(path, cwd).is_none());
    drop(failing);
    drop(declining);
}

#[tokio::test]
async fn wrapped_definition_executes_agent_contract() {
    let definition = ToolDefinition::new("echo", "echo", json!({}), Arc::new(|call| Box::pin(async move {
        call.signal.check()?;
        Ok(ToolResult::text(call.params["value"].as_str().unwrap_or_default()))
    })));
    let agent = maho_tools::tool_definition_wrapper::wrap_tool_definition(definition, None);
    let result = (agent.execute)("id".into(), json!({"value":"hello"}), None, None).await;
    assert_eq!(serde_json::to_value(result.content).unwrap()[0]["text"], "hello");
}
