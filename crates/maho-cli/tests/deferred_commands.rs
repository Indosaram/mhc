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
// senpi deferred-commands.ts: `dispatchConfigCommand`/`dispatchHostCommand`/`dispatchScheduleCommand`
// select on argv[0] alone; `host session` routes to the host-session command.
#[test]
fn host_schedule_and_config_routes_select_on_argv0() {
    let argv = |args: &[&str]| args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>();
    assert_eq!((CONFIG_COMMAND_ARGV, HOST_COMMAND_ARGV, SCHEDULE_COMMAND_ARGV), ("config", "host", "schedule"));
    assert_eq!(host_subcommand(&argv(&["host", "status"])), Some("status"));
    assert_eq!(host_subcommand(&argv(&["host"])), None);
    assert_eq!(host_subcommand(&argv(&["--", "host", "status"])), None);
    assert!(is_host_session_command_argv(&argv(&["host", "session", "list"])));
    assert!(!is_host_session_command_argv(&argv(&["host", "status"])));
    assert!(is_schedule_command_argv(&argv(&["schedule", "run", "--exec", "--watch"])));
    assert!(!is_schedule_command_argv(&argv(&["--schedule"])));
    assert!(!is_package_command_argv(&argv(&["--", "install"])));
}
