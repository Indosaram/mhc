//! Port of senpi packages/ai/test/google-shared-convert-tools.test.ts.

mod google_fixtures;

use google_fixtures::tool;
use maho_ai::api::google_shared::{
    convert_tools, resolve_google_function_calling_mode, supports_google_strict_tool_sampling,
    FunctionCallingConfigMode,
};
use maho_ai::types::{ConstrainedSampling, ConstrainedSamplingConfig, JsonSchemaStrictness};
use serde_json::{json, Value};

fn declaration(tools: &[maho_ai::types::Tool], use_parameters: bool) -> Value {
    let converted = convert_tools(tools, use_parameters, true).expect("convertTools succeeds");
    converted.expect("non-empty tool list converts")[0]["functionDeclarations"][0].clone()
}

fn strict_tool(parameters: Value) -> maho_ai::types::Tool {
    let mut tool = tool("test_tool", "A test tool", parameters);
    tool.constrained_sampling =
        Some(ConstrainedSampling::Config(ConstrainedSamplingConfig::JsonSchema {
            strict: JsonSchemaStrictness::Require,
        }));
    tool
}

#[test]
fn strips_json_schema_meta_keys_from_parameters_when_use_parameters_is_true() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "$id": "urn:bash-tool",
            "$comment": "A bash tool for demonstration",
            "$defs": { "commandDef": { "type": "string" } },
            "definitions": { "legacyDef": { "type": "number" } },
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"],
        }),
    )];

    let decl = declaration(&tools, true);

    assert_eq!(
        decl["parameters"],
        json!({ "type": "object", "properties": { "command": { "type": "string" } }, "required": ["command"] })
    );
    for key in ["$schema", "$id", "$comment", "$defs", "definitions"] {
        assert!(decl["parameters"].get(key).is_none(), "parameters must not carry {key}");
    }
}

#[test]
fn recursively_strips_nested_json_schema_meta_keys() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {
                "deep": { "$schema": "http://json-schema.org/draft-07/schema#", "$id": "urn:nested", "type": "string" }
            },
        }),
    )];

    let decl = declaration(&tools, true);

    assert_eq!(decl["parameters"], json!({ "type": "object", "properties": { "deep": { "type": "string" } } }));
}

#[test]
fn preserves_ref_while_stripping_meta_keys() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "refProp": { "$ref": "#/$defs/someDef", "type": "string" } },
        }),
    )];

    let decl = declaration(&tools, true);

    assert_eq!(
        decl["parameters"],
        json!({ "type": "object", "properties": { "refProp": { "$ref": "#/$defs/someDef", "type": "string" } } })
    );
}

#[test]
fn does_not_mutate_the_original_tool_parameters_object() {
    let original = json!({
        "$schema": "http://json-schema.org/draft-07/schema#",
        "type": "object",
        "properties": { "command": { "type": "string" } },
        "required": ["command"],
    });
    let tools = [tool("test_tool", "A test tool", original.clone())];

    let _ = convert_tools(&tools, true, true).expect("convertTools succeeds");

    assert_eq!(tools[0].parameters, original);
}

#[test]
fn preserves_schema_in_parameters_json_schema_when_use_parameters_is_false() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"],
        }),
    )];

    let decl = declaration(&tools, false);

    assert_eq!(
        decl["parametersJsonSchema"],
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"],
        })
    );
}

#[test]
fn strips_non_standard_optional_keyword_from_parameters_json_schema_when_use_parameters_is_false() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "command": { "type": "string", "optional": true } },
            "required": ["command"],
        }),
    )];

    let decl = declaration(&tools, false);

    assert_eq!(
        decl["parametersJsonSchema"],
        json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "command": { "type": "string" } },
            "required": ["command"],
        })
    );
}

#[test]
fn handles_tools_without_schema_gracefully() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] }),
    )];

    let decl = declaration(&tools, true);

    assert_eq!(
        decl["parameters"],
        json!({ "type": "object", "properties": { "path": { "type": "string" } }, "required": ["path"] })
    );
}

#[test]
fn uses_validated_function_calling_for_strict_tools_on_gemini_3() {
    let tool = strict_tool(json!({ "type": "object", "properties": {} }));

    assert!(supports_google_strict_tool_sampling("gemini-3.1-pro-preview"));
    assert!(!supports_google_strict_tool_sampling("gemini-2.5-pro"));
    assert_eq!(
        resolve_google_function_calling_mode(std::slice::from_ref(&tool), None, true).expect("strict mode resolves"),
        Some(FunctionCallingConfigMode::Validated)
    );
    let error = resolve_google_function_calling_mode(std::slice::from_ref(&tool), None, false)
        .expect_err("strict tools need strict-mode support");
    assert!(
        error.starts_with("Tool \"test_tool\" requires JSON-schema constrained sampling"),
        "unexpected error text: {error}"
    );
}

#[test]
fn returns_none_for_empty_tool_list() {
    assert!(convert_tools(&[], false, true).expect("converts").is_none());
    assert!(convert_tools(&[], true, true).expect("converts").is_none());
}

#[test]
fn preserves_a_legitimate_property_named_optional_in_parameters_json_schema_when_use_parameters_is_false() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({
            "type": "object",
            "properties": { "optional": { "type": "boolean", "description": "legitimate parameter" } },
            "required": ["optional"],
        }),
    )];

    let decl = declaration(&tools, false);

    assert_eq!(
        decl["parametersJsonSchema"],
        json!({
            "type": "object",
            "properties": { "optional": { "type": "boolean", "description": "legitimate parameter" } },
            "required": ["optional"],
        })
    );
}

#[test]
fn preserves_a_legitimate_property_named_optional_in_parameters_when_use_parameters_is_true() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({
            "type": "object",
            "properties": { "optional": { "type": "boolean", "description": "legitimate parameter" } },
            "required": ["optional"],
        }),
    )];

    let decl = declaration(&tools, true);

    assert_eq!(
        decl["parameters"],
        json!({
            "type": "object",
            "properties": { "optional": { "type": "boolean", "description": "legitimate parameter" } },
            "required": ["optional"],
        })
    );
}

#[test]
fn preserves_object_valued_const_default_examples_containing_optional_key_in_parameters_json_schema() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "object",
                    "const": { "optional": true, "keep": 1 },
                    "default": { "optional": false, "keep": 2 },
                    "examples": [{ "optional": true, "keep": 3 }],
                }
            },
        }),
    )];

    let decl = declaration(&tools, false);

    assert_eq!(
        decl["parametersJsonSchema"],
        json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "object",
                    "const": { "optional": true, "keep": 1 },
                    "default": { "optional": false, "keep": 2 },
                    "examples": [{ "optional": true, "keep": 3 }],
                }
            },
        })
    );
}

#[test]
fn strips_optional_from_any_of_branches_on_both_use_parameters_paths() {
    let tools = [tool(
        "test_tool",
        "A test tool",
        json!({ "anyOf": [{ "type": "string", "optional": true }, { "type": "number" }] }),
    )];

    let parameters_json_schema = declaration(&tools, false);
    assert_eq!(
        parameters_json_schema["parametersJsonSchema"],
        json!({ "anyOf": [{ "type": "string" }, { "type": "number" }] })
    );

    let parameters = declaration(&tools, true);
    assert_eq!(parameters["parameters"], json!({ "anyOf": [{ "type": "string" }, { "type": "number" }] }));
}
