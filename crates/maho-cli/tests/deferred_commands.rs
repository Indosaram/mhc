use maho_cli::cli::{deferred_commands::*, grok_neo_gate::*};
#[test]
fn resource_routes_accept_only_known_verbs() {
    for verb in ["install", "remove", "update", "list", "uninstall"] { assert!(is_package_command_argv(&[verb.to_owned()])); }
    assert!(!is_package_command_argv(&["config".to_owned()]));
    assert!(!is_package_command_argv(&[]));
}
#[test]
fn grok_gate_requires_explicit_truthy_value() {
    assert!(!is_grok_neo_enabled(&Default::default()));
    let env = std::collections::HashMap::from([("MAHO_ENABLE_GROK_NEO".to_owned(), " YeS ".to_owned())]);
    assert!(is_grok_neo_enabled(&env));
}
