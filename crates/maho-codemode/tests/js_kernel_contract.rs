use maho_codemode::kernels::js::kernel_contract::*;
use std::sync::{Arc, Mutex};

#[test]
fn closing_and_closed_refuse_each_operation() {
    for state in [LifecycleState::Closing, LifecycleState::Closed] {
        for operation in [KernelOperation::Run, KernelOperation::Reset, KernelOperation::Interrupt] {
            let error = assert_javascript_kernel_open(state, operation).unwrap_err();
            assert_eq!(error.operation, operation);
            assert_eq!(error.to_string(), format!("Cannot {operation}: JavaScript kernel is closed"));
        }
    }
    assert!(assert_javascript_kernel_open(LifecycleState::Open, KernelOperation::Run).is_ok());
}

#[test]
fn live_tool_names_are_resolved_fresh() {
    let names = Arc::new(Mutex::new(vec!["read".into()]));
    let source_names = Arc::clone(&names);
    let source = KernelToolNameSource::Provider(Arc::new(move || source_names.lock().unwrap().clone()));
    assert_eq!(resolve_kernel_tool_name_source(Some(&source)), ["read"]);
    names.lock().unwrap().push("write".into());
    assert_eq!(resolve_kernel_tool_name_source(Some(&source)), ["read", "write"]);
    assert!(resolve_kernel_tool_name_source(None).is_empty());
}
