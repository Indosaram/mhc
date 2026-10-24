use maho_ext_permission_system::{
    cli::parse_permission_preset_name, config::{rules_for_preset_in, PresetContext}, evaluate::evaluate, events::PermissionEventEmitter,
    external_dir::{extract_external_paths, is_credential_path}, non_interactive::handle_no_ui, types::*,
};
use std::path::Path;

fn context() -> PresetContext { PresetContext { agent_dir: "/home/u/.mhc/agent".into(), temp_dir: "/tmp".into() } }
fn guarded() -> Ruleset { rules_for_preset_in(PermissionPresetName::Guarded, &context()) }
fn request(permission: &str, pattern: &str) -> Request {
    Request { id: "g-1".into(), session_id: "s".into(), permission: permission.into(), patterns: vec![pattern.into()], always: vec![], metadata: Default::default(), tool: None }
}

#[test]
fn guarded_parses_and_round_trips_through_serde_for_handover() {
    assert_eq!(parse_permission_preset_name("guarded"), Some(PermissionPresetName::Guarded));
    assert_eq!(serde_json::to_value(PermissionPresetName::Guarded).expect("json"), serde_json::json!("guarded"));
    let back: PermissionPresetName = serde_json::from_value(serde_json::json!("guarded")).expect("parse");
    assert_eq!(back, PermissionPresetName::Guarded);
}

#[test]
fn guarded_allows_mhc_owned_paths_and_asks_for_credentials_and_system_temp() {
    let rules = guarded();
    assert_eq!(evaluate("external_directory", "/home/u/.mhc/agent/sessions/*", &[&rules]).action, Action::Allow);
    assert_eq!(evaluate("external_directory", "/home/u/.mhc/memory/*", &[&rules]).action, Action::Allow);
    assert_eq!(evaluate("external_directory", "/home/u/.mhc/agent/*", &[&rules]).action, Action::Ask);
    assert_eq!(evaluate("credential_file", "/home/u/.mhc/agent/auth.json", &[&rules]).action, Action::Ask);
    assert_eq!(evaluate("external_directory", "/tmp/*", &[&rules]).action, Action::Ask);
}

#[test]
fn credential_files_are_detected_even_when_cwd_is_the_agent_dir() {
    let agent = Path::new("/home/u/.mhc/agent");
    let home = Path::new("/home/u");
    assert!(is_credential_path("auth.json", agent, home, agent));
    assert!(is_credential_path("./auth.json.lock", agent, home, agent));
    assert!(is_credential_path("/home/u/.mhc/agent/auth.json", Path::new("/work"), home, agent));
    assert!(!is_credential_path("settings.json", agent, home, agent));
    assert!(!is_credential_path("auth.json", Path::new("/work"), home, agent));
    // Headless: the credential request is refused, not silently allowed.
    let rules = guarded();
    let outcome = handle_no_ui(&request("credential_file", "auth.json"), (&rules, &[]), &PermissionEventEmitter::default()).expect("emit").expect("rejection");
    assert_eq!(outcome.reply, Reply::Reject);
}

#[test]
fn headless_external_access_is_rejected_with_actionable_message() {
    let rules = guarded();
    let outcome = handle_no_ui(&request("external_directory", "/etc/passwd"), (&rules, &[]), &PermissionEventEmitter::default()).expect("emit").expect("rejection");
    assert_eq!(outcome.message.as_deref(), Some("Permission required to access /etc/passwd outside the working directory. Allow it with --permission external_directory=allow, set permissionPreset to \"full-access\", or add a permission rule."));
}

#[test]
fn headless_cli_override_allows_external_access() {
    let rules = guarded();
    let cli = [Rule { permission: "external_directory".into(), pattern: "*".into(), action: Action::Allow }];
    assert!(handle_no_ui(&request("external_directory", "/etc/passwd"), (&rules, &cli), &PermissionEventEmitter::default()).expect("emit").is_none());
}

#[test]
fn read_of_auth_json_with_cwd_equal_to_agent_dir_is_refused_headlessly_through_the_registry() {
    let agent = tempfile::tempdir().expect("agent dir");
    let home = Path::new("/home/u");
    let registry = maho_ext_permission_system::parsers::create_builtin_parser_registry_in(agent.path());
    let requests = registry.parse("read", &serde_json::json!({ "path": "auth.json" }), (agent.path(), home));
    let credential = requests.iter().find(|request| request.permission == "credential_file").expect("credential_file request for auth.json");
    let rules = rules_for_preset_in(maho_ext_permission_system::config::DEFAULT_PERMISSION_PRESET, &PresetContext { agent_dir: agent.path().into(), temp_dir: "/tmp".into() });
    let outcome = handle_no_ui(&request("credential_file", &credential.patterns[0]), (&rules, &[]), &PermissionEventEmitter::default()).expect("emit").expect("rejection");
    assert_eq!(outcome.reply, Reply::Reject);
    // The same file is not a credential under a different session agent dir.
    let other = tempfile::tempdir().expect("other dir");
    let registry = maho_ext_permission_system::parsers::create_builtin_parser_registry_in(other.path());
    assert!(registry.parse("read", &serde_json::json!({ "path": "auth.json" }), (agent.path(), home)).iter().all(|request| request.permission != "credential_file"));
}

#[test]
fn shell_detection_limits_are_documented_best_effort() {
    // IS-1 best-effort statement: only literal path-like tokens are inspected. Indirection is NOT detected.
    let cwd = Path::new("/workspace/project");
    let home = Path::new("/home/u");
    assert!(extract_external_paths("FOO=/etc/x cat $FOO", cwd, home).is_empty());
    assert!(extract_external_paths("python -c 'open(\"/etc/x\")'", cwd, home).is_empty());
    assert_eq!(extract_external_paths("cat /etc/passwd", cwd, home), vec!["/etc/passwd"]);
}
