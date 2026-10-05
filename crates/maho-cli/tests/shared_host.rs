use maho_cli::cli::shared_host::{
    is_truthy_env_flag, should_join_shared_host, shared_host_socket, EXPERIMENTAL_SHARED_HOST_SETTING,
    SHARED_HOST_ENABLE_ENV_SUFFIX,
};
use maho_core::project_trust::AppMode;

#[test]
fn shared_host_is_off_unless_interactive_and_opted_in() {
    assert!(!should_join_shared_host(AppMode::Print, true, true));
    assert!(!should_join_shared_host(AppMode::Rpc, true, true));
    assert!(!should_join_shared_host(AppMode::Json, true, true));
    assert!(!should_join_shared_host(AppMode::AppServer, true, true));
    assert!(!should_join_shared_host(AppMode::Interactive, false, false));
    assert!(should_join_shared_host(AppMode::Interactive, true, false));
    assert!(should_join_shared_host(AppMode::Interactive, false, true));
}

#[test]
fn truthy_env_flag_matches_the_pinned_values() {
    for value in ["1", "true", "TRUE", "True", "yes", "YES"] {
        assert!(is_truthy_env_flag(Some(value)), "{value}");
    }
    for value in ["", "0", "false", "no", "on", "2"] {
        assert!(!is_truthy_env_flag(Some(value)), "{value}");
    }
    assert!(!is_truthy_env_flag(None));
}

#[test]
fn socket_falls_back_to_the_agent_rpc_socket() {
    let env = maho_core::config::current_env();
    let configured = maho_core::brand::env_value("RPC_SOCKET", &env).filter(|value| !value.is_empty());
    let resolved = shared_host_socket("/tmp/agent");
    match configured {
        Some(value) => assert_eq!(resolved, value),
        None => assert_eq!(resolved, "/tmp/agent/rpc/rpc.sock"),
    }
}

#[test]
fn shared_host_constants_match_the_pinned_names() {
    assert_eq!(SHARED_HOST_ENABLE_ENV_SUFFIX, "ENABLE_SHARED_HOST");
    assert_eq!(EXPERIMENTAL_SHARED_HOST_SETTING, "experimentalSharedHost");
}
