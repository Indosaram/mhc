use maho_codemode::bridges::schema_bridge::*;
use serde_json::json;

fn tools() -> Vec<EvalSchemaToolInfo> {
    vec![
        EvalSchemaToolInfo {
            name: "mcp_computer_use_batch".into(),
            description: Some("Run steps.".into()),
            parameters: Some(json!({"type":"object", "required":["steps"]})),
            kernel_prelude: None,
        },
        EvalSchemaToolInfo {
            name: "grep".into(),
            description: None,
            parameters: None,
            kernel_prelude: None,
        },
    ]
}

#[test]
fn known_tool_returns_its_schema() {
    let result = run_eval_schema(&json!({"name":"mcp_computer_use_batch"}), &tools()).unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap()["parameters"]["required"],
        json!(["steps"])
    );
}

#[test]
fn absent_name_lists_tools_in_order() {
    assert_eq!(
        run_eval_schema(&json!({}), &tools()).unwrap(),
        EvalSchemaResult::Tools {
            tools: vec!["mcp_computer_use_batch".into(), "grep".into()]
        }
    );
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
    assert!(matches!(
        run_eval_schema(&json!({"nope":1}), &tools()),
        Err(SchemaError::Arguments(_))
    ));
}

#[test]
fn invalid_names_and_nonobjects_are_rejected() {
    for value in [
        json!(null),
        json!([]),
        json!({"name":""}),
        json!({"name":2}),
    ] {
        assert!(matches!(
            run_eval_schema(&value, &tools()),
            Err(SchemaError::Arguments(_))
        ));
    }
}

#[test]
fn absent_description_and_parameters_are_omitted() {
    let result = run_eval_schema(&json!({"name":"grep"}), &tools()).unwrap();
    assert_eq!(
        serde_json::to_value(result).unwrap(),
        json!({"name":"grep"})
    );
}

#[test]
fn reserved_names_stay_distinct_and_recognized() {
    use maho_codemode::bridge::reserved::{
        RESERVED_AGENT_TOOL, RESERVED_OUTPUT_TOOL, RESERVED_SCHEMA_TOOL,
    };
    use maho_codemode::bridges::reserved_dispatch::is_reserved_tool_name;
    assert_eq!(RESERVED_SCHEMA_TOOL, "__schema__");
    let names: std::collections::HashSet<_> = [
        RESERVED_AGENT_TOOL,
        RESERVED_OUTPUT_TOOL,
        RESERVED_SCHEMA_TOOL,
    ]
    .into_iter()
    .collect();
    assert_eq!(names.len(), 3);
    assert!(is_reserved_tool_name(RESERVED_SCHEMA_TOOL));
    assert!(!is_reserved_tool_name("grep"));
}

// typebox 1.3.34 `Errors` summaries captured at senpi 903b8336 (schema-bridge.ts parseSchemaArgs).
#[test]
fn invalid_arguments_report_typebox_errors_in_order() {
    for (value, summary) in [
        (
            json!({"name":""}),
            "/name must not have fewer than 1 characters",
        ),
        (json!({"name":3}), "/name must be string"),
        (
            json!({"name":"","extra":1,"z":2}),
            "/extra schema is false; /z schema is false; / must not have additional properties; /name must not have fewer than 1 characters",
        ),
        (json!(null), "/ must be object"),
    ] {
        let error = run_eval_schema(&value, &tools()).expect_err("invalid arguments");
        assert_eq!(
            error.to_string(),
            format!("schema() received invalid arguments: {summary}")
        );
    }
}

// eval-virtual-schemas.ts: virtual entries resolve by name and never join the catalog listing.
#[test]
fn virtual_eval_entries_resolve_but_are_never_listed() {
    let listed = run_eval_schema(&json!({}), &tools()).unwrap();
    let EvalSchemaResult::Tools { tools: names } = listed else {
        panic!("expected listing")
    };
    assert!(names.iter().all(|name| !name.starts_with("eval:")));
    assert_eq!(
        maho_codemode::bridges::eval_virtual_schemas::virtual_eval_schema_names(),
        [
            "eval:wait",
            "eval:helpers",
            "eval:kernel-tools",
            "eval:environments",
            "eval:isolation"
        ]
    );
    let EvalSchemaResult::Tool {
        name,
        description,
        parameters,
    } = run_eval_schema(&json!({"name":"eval:wait"}), &tools()).unwrap()
    else {
        panic!("expected virtual entry")
    };
    assert_eq!(name, "eval:wait");
    assert!(
        description
            .unwrap()
            .starts_with("wait(handles, {timeout?, mode?})")
    );
    assert_eq!(parameters.unwrap()["required"], json!(["handles"]));
}
