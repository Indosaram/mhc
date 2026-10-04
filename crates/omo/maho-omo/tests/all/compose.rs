use std::sync::{Arc, Mutex};
use maho_ext_api::{Extension, FlagValue};
use maho_omo::{OmoExtension, OmoSenpiComponent, OMO_DISABLED_FLAG};

#[test]
fn components_receive_one_runtime_and_see_earlier_captured_tools() {
    let shared = Arc::new(Mutex::new(Vec::new()));
    let first = shared.clone();
    let second = shared.clone();
    let extension = OmoExtension::new(vec![
        OmoSenpiComponent::from_context_register("first", move |api, runtime| {
            first.lock().unwrap().push(runtime.context().idle_coordinator.clone());
            api.register_tool(crate::support::fake_tool("first"));
        }),
        OmoSenpiComponent::from_context_register("second", move |_, runtime| {
            assert_eq!(runtime.captured_tools()[0].name, "first");
            second.lock().unwrap().push(runtime.context().idle_coordinator.clone());
        }),
    ]);
    let mut api = crate::support::new_api();
    extension.register(&mut api);
    let shared = shared.lock().unwrap();
    assert_eq!(shared.len(), 2);
    assert!(Arc::ptr_eq(&shared[0], &shared[1]));
}

#[test]
fn disabled_reregistration_clears_the_previous_runtime() {
    let extension = OmoExtension::new(Vec::new());
    let mut api = crate::support::new_api();
    extension.register(&mut api);
    assert!(extension.runtime().is_some());
    api.set_flag(OMO_DISABLED_FLAG, FlagValue::Boolean(true));
    extension.register(&mut api);
    assert!(extension.runtime().is_none());
}
