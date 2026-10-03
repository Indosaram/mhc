#[path = "../examples/runtime_proof.rs"]
mod runtime_proof;

#[test]
fn registered_component_executes_native_runtime_and_releases_resources() {
    runtime_proof::main().expect("registered native runtime proof");
}
