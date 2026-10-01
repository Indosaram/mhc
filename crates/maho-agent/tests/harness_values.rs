//! Port of senpi packages/agent/test/harness/values.test.ts.

use maho_agent::harness::session::values::{
    branch_tip, branch_tip_inventory_prefix, entry_label, lane_config, lane_state, list, operation_meta,
    operation_preparation, operation_preparation_prefix, operation_result, operation_state, operation_tool_args,
    operation_tool_args_prefix, operation_tool_memo, operation_tool_memo_prefix, pending_assistant_frames,
    pending_entry, pending_tool_output, pending_tool_output_prefix, session_name, value,
};

#[test]
fn binds_immutable_scalar_and_list_addresses_with_validated_components() {
    assert_eq!(value("app.state", "").expect("address").namespace, "app.state");
    assert_eq!(list("app.events", "workspace").expect("address").key, "workspace");
    assert!(value("", "").is_err());
    assert_eq!(
        value("", "").expect_err("empty namespace").message,
        "Value namespace must not be empty"
    );
    assert_eq!(
        value("app\u{0}state", "").expect_err("nul namespace").message,
        "Value namespace must not contain \\u0000"
    );
    assert_eq!(
        list("app.events", "bad\u{0}key").expect_err("nul key").message,
        "Value key must not contain \\u0000"
    );
    assert_eq!(value("pi.application", "").expect("address").namespace, "pi.application");
}

#[test]
fn uses_separately_constructed_equal_addresses_for_one_durable_location() {
    let first = value("app.state", "workspace").expect("address");
    let second = value("app.state", "workspace").expect("address");
    assert_eq!(first, second);
    assert_ne!(first, value("app.state", "other").expect("address"));
}

#[test]
fn uses_the_exact_reserved_namespaces_keys_and_value_types() {
    let addresses = [
        branch_tip("review"),
        lane_config("review"),
        lane_state("review"),
        operation_result("operation"),
        operation_meta("operation"),
        operation_state("operation"),
        operation_tool_args("operation", "step", 2),
        operation_tool_memo("operation", "invocation", "name"),
        operation_preparation("operation", "task"),
        pending_entry("entry"),
        pending_tool_output("operation", "invocation"),
        session_name(),
        entry_label("entry"),
    ];
    let expected = [
        ("pi.branch.tip", "review"),
        ("pi.lane.config", "review"),
        ("pi.lane.state", "review"),
        ("pi.result", "operation"),
        ("pi.op.meta", "operation"),
        ("pi.op.state", "operation"),
        ("pi.op.tool_args", "operation:step:2"),
        ("pi.op.tool_memo", "operation:invocation:name"),
        ("pi.op.preparation", "operation:task"),
        ("pi.pending.entry", "entry"),
        ("pi.pending.tool_output", "operation:invocation"),
        ("pi.session.name", ""),
        ("pi.entry.label", "entry"),
    ];
    for (address, (namespace, key)) in addresses.iter().zip(expected) {
        assert_eq!(address.namespace, namespace);
        assert_eq!(address.key, key);
    }
    assert_eq!(
        pending_assistant_frames("operation", "response"),
        list("pi.pending.assistant_frame", "operation:response").expect("address")
    );
}

#[test]
fn exports_exactly_the_five_documented_scan_prefix_constructors() {
    let prefixes = [
        branch_tip_inventory_prefix(),
        operation_tool_args_prefix("operation", None),
        operation_tool_memo_prefix("operation", None),
        operation_preparation_prefix("operation"),
        pending_tool_output_prefix("operation"),
    ];
    let expected = [
        ("pi.branch.tip", ""),
        ("pi.op.tool_args", "operation:"),
        ("pi.op.tool_memo", "operation:"),
        ("pi.op.preparation", "operation:"),
        ("pi.pending.tool_output", "operation:"),
    ];
    for (prefix, (namespace, key)) in prefixes.iter().zip(expected) {
        assert_eq!(prefix.namespace, namespace);
        assert_eq!(prefix.key, key);
    }
    assert_eq!(
        operation_tool_args_prefix("operation", Some("step")).key,
        "operation:step:"
    );
    assert_eq!(operation_tool_memo_prefix("operation", Some("invocation")).key, "operation:invocation:");
}
