use lsp_core::lsp::server_resolution::find_server_for_extension;
use lsp_core::lsp::types::ServerLookupResult;
use lsp_core::request_context::{run_with_request_context,LspRequestContext,LspRequestCapabilities};
use std::{fs,path::Path};

fn context(root:&Path)->LspRequestContext {
    LspRequestContext{cwd:root.to_string_lossy().into_owned(),project_config_paths:vec![root.join("lsp.json").to_string_lossy().into_owned()],user_config_path:root.join("user.json").to_string_lossy().into_owned(),install_decisions_path:root.join("decisions.json").to_string_lossy().into_owned(),capabilities:LspRequestCapabilities{install_decision_tool:true}}
}

#[test]
fn local_binary_substitution_preserves_arguments_environment_and_initialization() {
    let root=tempfile::tempdir().unwrap();
    fs::write(root.path().join("package.json"),"{}").unwrap();
    let dir=root.path().join("node_modules").join(".bin");
    fs::create_dir_all(&dir).unwrap();
    let binary=dir.join("native-parity-test-lsp");
    fs::write(&binary,"#!/bin/sh\nexit 0\n").unwrap();
    #[cfg(unix)] {use std::os::unix::fs::PermissionsExt;fs::set_permissions(&binary,fs::Permissions::from_mode(0o755)).unwrap();}
    fs::write(root.path().join("user.json"),serde_json::json!({"lsp":{"native-test":{"command":["native-parity-test-lsp","--stdio","--argument"],"extensions":[".native-test"],"env":{"KEEP":"yes"},"initialization":{"feature":true}}}}).to_string()).unwrap();
    let result=run_with_request_context(context(root.path()),||find_server_for_extension(".native-test")).unwrap();
    let ServerLookupResult::Found{server}=result else{panic!("expected repo-local server")};
    assert_eq!(server.command,vec![binary.to_string_lossy().into_owned(),"--stdio".into(),"--argument".into()]);
    assert_eq!(server.env.unwrap()["KEEP"],"yes");
    assert_eq!(server.initialization.unwrap()["feature"],true);
}

#[test]
fn missing_configured_binary_and_unknown_extension_keep_distinct_results() {
    let root=tempfile::tempdir().unwrap();
    fs::write(root.path().join("user.json"),serde_json::json!({"lsp":{"native-test":{"command":["native-parity-missing-binary"],"extensions":[".native-test"]}}}).to_string()).unwrap();
    let result=run_with_request_context(context(root.path()),||find_server_for_extension(".native-test")).unwrap();
    assert!(matches!(result,ServerLookupResult::NotInstalled{server,..} if server.id=="native-test"));
    let result=run_with_request_context(context(root.path()),||find_server_for_extension(".native-unknown")).unwrap();
    assert!(matches!(result,ServerLookupResult::NotConfigured{..}));
}
