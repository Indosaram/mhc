use maho_cli::experimental::process::*;
#[test]
fn explicit_legacy_directory_lane_wins_without_altering_unrelated_env() {
    let mut env = std::collections::BTreeMap::from([("PI_CODING_AGENT_DIR".to_owned(), "chosen".to_owned()), ("MAHO_CODING_AGENT_DIR".to_owned(), "other".to_owned()), ("KEEP".to_owned(), "value".to_owned())]);
    normalize_agent_dir_lane(&mut env);
    assert_eq!(env.len(), 2); assert_eq!(env["PI_CODING_AGENT_DIR"], "chosen");
}
#[test]
fn consumption_validates_role_before_removing_it() {
    let mut env = std::collections::BTreeMap::from([(INTERNAL_PROCESS_ENV.to_owned(), "server".to_owned())]);
    assert!(consume_internal_process_role(&mut env).unwrap() == Some(InternalProcessRole::Server));
    assert!(!env.contains_key(INTERNAL_PROCESS_ENV));
    env.insert(INTERNAL_PROCESS_ENV.to_owned(), "invalid".to_owned());
    assert!(consume_internal_process_role(&mut env).is_err());
    assert!(env.contains_key(INTERNAL_PROCESS_ENV));
}
