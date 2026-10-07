use maho_cli::cli::shared_host::{
    is_truthy_env_flag, session_profile, should_join_shared_host, shared_host_socket, EXPERIMENTAL_SHARED_HOST_SETTING,
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

#[test]
fn session_profile_keeps_the_shared_host_off_for_a_classic_launch() {
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().into_owned();
    let settings = maho_core::settings_manager::SettingsManager::create(&dir, &dir, &dir, false);
    for mode in [AppMode::Print, AppMode::Rpc, AppMode::Json, AppMode::AppServer] {
        let profile = session_profile(mode, &settings, None, None);
        assert!(!profile.shared_host_enabled, "{mode:?}");
        assert_eq!(profile.session_kind, maho_ext_api::SessionKind::Interactive);
        assert!(profile.session_context.is_empty());
    }
    let context = std::collections::BTreeMap::from([("role".to_owned(), "qa".to_owned())]);
    let profile = session_profile(AppMode::Print, &settings, Some(maho_ext_api::SessionKind::Worker), Some(context.clone()));
    assert!(!profile.shared_host_enabled);
    assert_eq!(profile.session_kind, maho_ext_api::SessionKind::Worker);
    assert_eq!(profile.session_context, context);
}

#[tokio::test]
async fn a_computed_profile_reaches_the_loaded_extension() {
    let captured: std::sync::Arc<std::sync::Mutex<Option<maho_ext_api::ExtensionSessionProfile>>> =
        std::sync::Arc::new(std::sync::Mutex::new(None));
    let slot = captured.clone();
    let factory: maho_ext_host::loader::AsyncExtensionFactory =
        std::sync::Arc::new(move |api: &mut maho_ext_api::ExtensionApi| {
            *slot.lock().unwrap() = Some(api.profile.clone());
            Box::pin(async { Ok(()) })
        });
    let temp = tempfile::tempdir().unwrap();
    let dir = temp.path().to_string_lossy().into_owned();
    let settings = maho_core::settings_manager::SettingsManager::create(&dir, &dir, &dir, false);
    let context = std::collections::BTreeMap::from([("role".to_owned(), "qa".to_owned())]);
    let profile = session_profile(AppMode::Print, &settings, Some(maho_ext_api::SessionKind::Worker), Some(context.clone()));
    let loaded = maho_ext_host::loader::load_extensions_async(
        vec![maho_ext_host::loader::NativeAsyncExtensionFactory {
            path: "<qa:profile>".to_owned(),
            source_info: maho_ext_api::SourceInfo { path: "<qa:profile>".to_owned(), source: "user".into(), ..Default::default() },
            factory,
        }],
        temp.path(),
        profile,
    ).await;
    assert!(loaded.errors.is_empty());
    assert_eq!(loaded.extensions.len(), 1);
    let seen = captured.lock().unwrap().clone().expect("the factory captured its profile");
    assert!(!seen.shared_host_enabled);
    assert_eq!(seen.session_kind, maho_ext_api::SessionKind::Worker);
    assert_eq!(seen.session_context, context);
}
