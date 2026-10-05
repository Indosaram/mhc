//! Port of the capture half of `omo-senpi/src/extension/tool-capture-registry.test.ts`.

use maho_omo::ToolCaptureRegistry;
use crate::support::{fake_tool, new_api};

#[test]
fn every_registered_tool_is_captured_with_its_execute_closure_in_order() {
    let mut api = new_api();
    api.register_tool(fake_tool("lsp_diagnostics"));
    api.register_tool(fake_tool("lsp_find_references"));
    api.register_tool(fake_tool("task"));
    api.register_tool(fake_tool("task_send"));

    let registry = ToolCaptureRegistry::new();
    registry.capture(api.registered.tools.iter().map(|tool| tool.definition.clone()).collect());

    let captured = registry.get_captured_tools();
    assert_eq!(
        captured.iter().map(|tool| tool.name.clone()).collect::<Vec<_>>(),
        vec![
            "lsp_diagnostics".to_owned(),
            "lsp_find_references".to_owned(),
            "task".to_owned(),
            "task_send".to_owned(),
        ]
    );
    assert!(captured.iter().all(|tool| tool.parameters == serde_json::json!({ "type": "object" })));
}

#[test]
fn an_empty_registration_set_captures_nothing() {
    let registry = ToolCaptureRegistry::new();

    assert!(registry.get_captured_tools().is_empty());
}
