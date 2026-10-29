use std::{collections::BTreeMap, sync::{Arc, Mutex}, time::Duration};
use maho_ext_mcp::{config_schema::*, errors::{McpError, McpErrorKind}, log::McpLogger, stdio_env::is_filtered_env_key, transport::create_mcp_transport_spec, transport_sdk::McpClient};
use crate::support;

fn host_env() -> BTreeMap<String, String> {
    ["NPM_CONFIG_REGISTRY", "yarn_cache", "PNPM_HOME", "NO_UPDATE_NOTIFIER", "AWS_ACCESS_KEY_ID", "AWS_REGION_TOKEN", "AZURE_TENANT", "GCP_X", "KUBECONFIG", "DATABASE_URL", "GITHUB_TOKEN", "MY_API_KEY", "DB_PASSWORD", "SVC_CREDENTIALS", "Some_Secret", "KEEP_ME", "LANG_HINT"]
        .into_iter()
        .map(|key| (key.to_owned(), "v".to_owned()))
        .collect()
}
fn config(args: Vec<String>) -> McpServerConfig {
    McpServerConfig {
        enabled: Some(true),
        transport: Some(Transport::Stdio),
        command: Some(support::node()),
        args: Some(args),
        env: Some(BTreeMap::from([("CONFIGURED_TOKEN".into(), "kept".into()), ("CONFIGURED_KEY".into(), "kept".into())])),
        ..Default::default()
    }
}

#[test]
fn key_classifier_follows_the_documented_list() {
    for key in ["NPM_CONFIG_X", "npm_config_x", "YARN_X", "PNPM_X", "NO_UPDATE_NOTIFIER", "AWS_SECRET_ACCESS_KEY", "GOOGLE_APPLICATION_CREDENTIALS", "GOOGLE_CLOUD_PROJECT", "FIREBASE_X", "HEROKU_X", "VAULT_ADDR", "DOCKER_AUTH_CONFIG", "OPENAI_API_KEY", "A_KEY", "A_SECRET", "A_TOKEN", "A_PASSWORD", "A_CREDENTIAL", "A_CREDENTIALS", "A_API_KEY"] {
        assert!(is_filtered_env_key(key), "{key}");
    }
    for key in ["HOME", "PATH", "KEEP_ME", "TOKENIZER", "KEYBOARD", "AWS_REGION"] {
        assert!(!is_filtered_env_key(key), "{key}");
    }
}

#[tokio::test]
async fn stdio_server_sees_filtered_host_env_plus_unfiltered_configured_env() {
    let fixture = format!("{}/tests/fixtures/env-report.mjs", env!("CARGO_MANIFEST_DIR"));
    let spec = create_mcp_transport_spec("env", &config(vec![fixture]), Some(&host_env())).unwrap();
    let root = tempfile::tempdir().unwrap();
    let logger = Arc::new(Mutex::new(McpLogger::new("env", root.path(), None).unwrap()));
    let client = McpClient::materialize_stdio("env", &spec, logger).await.unwrap();
    let reply = tokio::time::timeout(Duration::from_secs(10), client.request("initialize", serde_json::json!({}), Duration::from_secs(10))).await.unwrap().unwrap();
    let keys: Vec<String> = reply["env"].as_array().unwrap().iter().map(|key| key.as_str().unwrap().to_owned()).collect();
    for kept in ["KEEP_ME", "LANG_HINT", "CONFIGURED_TOKEN", "CONFIGURED_KEY"] {
        assert!(keys.iter().any(|key| key == kept), "{kept} missing from {keys:?}");
    }
    for dropped in host_env().keys().filter(|key| is_filtered_env_key(key)) {
        assert!(!keys.contains(dropped), "{dropped} leaked");
    }
    client.close().await.unwrap();
}

#[tokio::test]
async fn diagnosis_rerun_applies_the_same_filter() {
    let script = "for (const key of Object.keys(process.env)) console.error(key)";
    let config = config(vec!["-e".into(), script.into()]);
    let root = tempfile::tempdir().unwrap();
    let logger = Arc::new(Mutex::new(McpLogger::new("env", root.path(), None).unwrap()));
    let error = tokio::time::timeout(Duration::from_secs(10), maho_ext_mcp::diagnose::diagnose_mcp_connect_failure("env", &config, Some(&host_env()), &McpError::new(McpErrorKind::Connect, "failed"), logger)).await.unwrap();
    assert!(error.message.contains("KEEP_ME"), "{}", error.message);
    assert!(error.message.contains("CONFIGURED_TOKEN"), "{}", error.message);
    assert!(!error.message.contains("MY_API_KEY"), "{}", error.message);
    assert!(!error.message.contains("NPM_CONFIG_REGISTRY"), "{}", error.message);
}
