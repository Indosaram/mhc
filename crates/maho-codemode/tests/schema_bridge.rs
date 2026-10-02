use maho_codemode::bridges::schema_bridge::*;
use serde_json::json;

fn tools() -> Vec<EvalSchemaToolInfo> {
    vec![EvalSchemaToolInfo { name: "mcp_computer_use_batch".into(), description: Some("Run steps.".into()), parameters: Some(json!({"type":"object", "required":["steps"]})) }, EvalSchemaToolInfo { name: "grep".into(), description: None, parameters: None }]
}

#[test]
fn known_tool_returns_its_schema() {
    let result = run_eval_schema(&json!({"name":"mcp_computer_use_batch"}), &tools()).unwrap();
    assert_eq!(serde_json::to_value(result).unwrap()["parameters"]["required"], json!(["steps"]));
}

#[test]
fn absent_name_lists_tools_in_order() {
    assert_eq!(run_eval_schema(&json!({}), &tools()).unwrap(), EvalSchemaResult::Tools { tools: vec!["mcp_computer_use_batch".into(), "grep".into()] });
}

#[test]
fn unknown_tools_suggest_partial_matches() {
    let error = run_eval_schema(&json!({"name":"mcp_computer_use_batchh"}), &tools()).unwrap_err();
    match error {
        SchemaError::UnknownTool { hint, .. } => assert!(hint.contains("mcp_computer_use_batch")),
        SchemaError::Arguments(_) => panic!("expected unknown tool"),
    }
}

#[test]
fn unexpected_arguments_are_rejected() {
    assert!(matches!(run_eval_schema(&json!({"nope":1}), &tools()), Err(SchemaError::Arguments(_))));
}

#[test]
fn invalid_names_and_nonobjects_are_rejected() {
    for value in [json!(null), json!([]), json!({"name":""}), json!({"name":2})] {
        assert!(matches!(run_eval_schema(&value, &tools()), Err(SchemaError::Arguments(_))));
    }
}

#[test]
fn absent_description_and_parameters_are_omitted() {
    let result = run_eval_schema(&json!({"name":"grep"}), &tools()).unwrap();
    assert_eq!(serde_json::to_value(result).unwrap(), json!({"name":"grep"}));
}
