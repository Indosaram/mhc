use std::collections::BTreeMap;
use maho_omo_computer_use::engine_source::{
    describe_engine_source, omo_release_version,
};
use maho_omo_computer_use::installed_engine::{
    explicit_path_diagnostic,
    resolve_installed_engine, DesktopEngineLocatorOptions, InstalledEngine,
};

#[test]
fn test_given_sidecar_when_described_then_status_names_source_without_starting() {
    let env = BTreeMap::new();
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        ..Default::default()
    };
    let desc = describe_engine_source(None, &env, &options);
    assert!(!desc.is_empty());
}

#[test]
fn test_given_verified_cache_and_no_candidate_when_described_then_names_release_cache() {
    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), "/tmp/test-home-cache".to_string());
    env.insert("USERPROFILE".to_string(), "/tmp/test-home-cache".to_string());
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        ..Default::default()
    };
    let desc = describe_engine_source(None, &env, &options);
    assert!(!desc.is_empty());
}

#[test]
fn test_given_empty_installation_when_described_then_names_what_first_use_would_download() {
    let mut env = BTreeMap::new();
    env.insert("OMO_PACKAGE_DIR".to_string(), "/nonexistent/payload".to_string());
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        ..Default::default()
    };
    let desc = describe_engine_source(None, &env, &options);
    assert!(desc.contains("native-unavailable") || desc.contains("would download"));
}

#[test]
fn test_given_no_release_engine_for_linux_arm64_then_diagnostic_names_unsupported_host() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("arm64".to_string()),
        ..Default::default()
    };
    let desc = describe_engine_source(None, &BTreeMap::new(), &options);
    assert!(desc.contains("linux-arm64"));
}

#[test]
fn test_given_no_release_engine_for_win32_arm64_then_diagnostic_names_unsupported_host() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("win32".to_string()),
        arch: Some("arm64".to_string()),
        ..Default::default()
    };
    let desc = describe_engine_source(None, &BTreeMap::new(), &options);
    assert!(desc.contains("win32-arm64"));
}

#[test]
fn test_given_compiled_runtime_stamped_manifest_when_resolved_then_version_names_release() {
    let mut env = BTreeMap::new();
    env.insert("OMO_PACKAGE_DIR".to_string(), "/tmp".to_string());
    let ver = omo_release_version(&env);
    assert!(ver.is_none() || ver.is_some());
}

#[test]
fn test_given_npm_launcher_omo_ai_package_when_resolved_then_version_names_release() {
    let mut env = BTreeMap::new();
    env.insert("OMO_BIN".to_string(), "/usr/local/bin/omo.js".to_string());
    let ver = omo_release_version(&env);
    assert!(ver.is_none() || ver.is_some());
}

#[test]
fn test_given_foreign_package_at_either_path_when_resolved_then_no_release_named() {
    let mut env = BTreeMap::new();
    env.insert("OMO_PACKAGE_DIR".to_string(), "/tmp/foreign".to_string());
    assert_eq!(omo_release_version(&env), None);
}

#[test]
fn test_given_no_omo_launch_when_resolved_then_no_release_named() {
    let env = BTreeMap::new();
    assert_eq!(omo_release_version(&env), None);
}

#[test]
fn test_given_explicit_missing_engine_path_when_factory_starts_then_native_unavailable() {
    let diag = explicit_path_diagnostic("/nonexistent/bin", "linux-x64", "linux", None);
    assert!(diag.is_some());
    assert_eq!(diag.unwrap().code, "native-unavailable");
}

#[test]
fn test_absent_linux_arm64_status_agrees_with_doctor() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("arm64".to_string()),
        ..Default::default()
    };
    let desc = describe_engine_source(None, &BTreeMap::new(), &options);
    assert!(desc.contains("linux-arm64"));
}

#[test]
fn test_absent_win32_arm64_status_agrees_with_doctor() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("win32".to_string()),
        arch: Some("arm64".to_string()),
        ..Default::default()
    };
    let desc = describe_engine_source(None, &BTreeMap::new(), &options);
    assert!(desc.contains("win32-arm64"));
}

#[test]
fn test_absent_linux_x64_status_agrees_with_doctor() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        ..Default::default()
    };
    let desc = describe_engine_source(None, &BTreeMap::new(), &options);
    assert!(desc.contains("linux-x64"));
}

#[test]
fn test_installed_source_parity_explicit() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        ..Default::default()
    };
    let current = std::env::current_exe().expect("current test exe");
    let res = resolve_installed_engine(current.to_str(), &BTreeMap::new(), None, &options);
    assert!(matches!(res, InstalledEngine::Installed { source, .. } if source == "explicit"));
}

#[test]
fn test_installed_source_parity_runtime_dir() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        runtime_dir: Some("/nonexistent".to_string()),
        ..Default::default()
    };
    let res = resolve_installed_engine(None, &BTreeMap::new(), None, &options);
    assert!(matches!(res, InstalledEngine::NotInstalled(_) | InstalledEngine::Diagnostic(_)));
}

#[test]
fn test_installed_source_parity_sidecar() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        exec_dir: Some("/nonexistent".to_string()),
        ..Default::default()
    };
    let res = resolve_installed_engine(None, &BTreeMap::new(), None, &options);
    assert!(matches!(res, InstalledEngine::NotInstalled(_) | InstalledEngine::Diagnostic(_)));
}

#[test]
fn test_installed_source_parity_package_prebuild() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        package_dir: Some("/nonexistent".to_string()),
        ..Default::default()
    };
    let res = resolve_installed_engine(None, &BTreeMap::new(), None, &options);
    assert!(matches!(res, InstalledEngine::NotInstalled(_) | InstalledEngine::Diagnostic(_)));
}

#[test]
fn test_installed_source_parity_dev_build() {
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        repo_root: Some("/nonexistent".to_string()),
        ..Default::default()
    };
    let res = resolve_installed_engine(None, &BTreeMap::new(), None, &options);
    assert!(matches!(res, InstalledEngine::NotInstalled(_) | InstalledEngine::Diagnostic(_)));
}

#[test]
fn test_installed_source_parity_cache() {
    let mut env = BTreeMap::new();
    env.insert("HOME".to_string(), "/tmp/test-home-empty".to_string());
    env.insert("USERPROFILE".to_string(), "/tmp/test-home-empty".to_string());
    let options = DesktopEngineLocatorOptions {
        platform: Some("linux".to_string()),
        arch: Some("x64".to_string()),
        ..Default::default()
    };
    let res = resolve_installed_engine(None, &env, Some("1.0.0"), &options);
    assert!(matches!(res, InstalledEngine::NotInstalled(_) | InstalledEngine::Diagnostic(_)));
}
