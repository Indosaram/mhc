use super::*;
use serde_json::json;
use std::io::Write;

fn grant(name: &str) -> KernelToolGrant {
    KernelToolGrant {
        capability: KernelToolsCapability::default(),
        descriptors: vec![KernelToolDescriptor {
            name: name.into(),
            description: "fixture".into(),
            input_schema: serde_json::Map::new(),
            language: KernelToolLanguage::Js,
            kernel_generation: 1,
            definition_revision: 1,
        }],
        requested_names: vec![name.into()],
        scope: None,
    }
}

#[test]
fn revived_same_host_child_keeps_its_binding() {
    let mut bindings = create_kernel_tool_bindings();
    bindings.bind("st_00000901", grant("fixture_lookup"));
    assert!(bindings.get("st_00000901").is_some());
    assert_eq!(bindings.get("st_00000901").unwrap().requested_names, ["fixture_lookup"]);
}

#[test]
fn reset_binding_is_stale_until_explicitly_released() {
    let mut bindings = create_kernel_tool_bindings();
    bindings.bind("child", grant("fixture_lookup"));
    bindings.release("child");
    assert!(bindings.get("child").is_none());
}

#[test]
fn redefining_a_name_does_not_mutate_the_old_descriptor() {
    let old = grant("fixture_lookup");
    let mut bindings = create_kernel_tool_bindings();
    bindings.bind("old", old.clone());
    bindings.bind("new", grant("fixture_lookup"));
    assert_eq!(bindings.get("old").unwrap().descriptors, old.descriptors);
}

#[test]
fn a_new_host_without_a_binding_has_no_grant() {
    let bindings = create_kernel_tool_bindings();
    assert!(bindings.get("st_00000901").is_none());
}

#[test]
fn released_binding_is_unavailable() {
    let mut bindings = create_kernel_tool_bindings();
    bindings.bind("child", grant("fixture_lookup"));
    bindings.release("child");
    assert!(bindings.is_empty());
}

#[test]
fn live_shared_tool_name_is_not_recorded_as_a_kernel_tool() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "{}", json!({"type":"session","id":"child-session"})).unwrap();
    writeln!(file, "{}", json!({"type":"message","message":{"role":"toolResult","toolName":"shared_helper","content":[{"type":"text","text":"live"}]}})).unwrap();
    assert!(recorded_kernel_tool_names(file.path().to_str().unwrap()).is_empty());
}

#[test]
fn a_transcript_without_kernel_tool_envelopes_restores_nothing() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "{}", json!({"type":"session","id":"child-session"})).unwrap();
    writeln!(file, "{}", json!({"type":"message","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}})).unwrap();
    assert_eq!(recorded_kernel_tool_names(file.path().to_str().unwrap()), Vec::<String>::new());
}
