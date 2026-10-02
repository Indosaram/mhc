use std::collections::BTreeMap;
use maho_ext_mcp::{config::{load_mcp_config,normalize_server},config_schema::*,sharing_policy::*};
use serde_json::json;

fn resolved_template(variable:&str) {
    let root=tempfile::tempdir().expect("temporary config root");
    std::fs::write(root.path().join("mcp.json"),json!({"mcpServers":{"fx":{"type":"stdio","command":"server","args":[format!("${{{variable}}}")]}}}).to_string()).expect("write config");
    let env=BTreeMap::from([(variable.into(),"resolved".into())]);
    let config=load_mcp_config(LoadMcpConfigOptions {cwd:root.path(),agent_dir:root.path(),env:&env,project_trusted:true}).expect("resolve config");
    let config=config.servers["fx"].config.as_ref().expect("resolved server");
    assert_eq!(config.args.as_ref().expect("resolved arguments"),&["resolved"]);
    assert!(!shareable(config,None));
    assert!(serde_json::to_value(config).expect("serialize config").get("sessionDependent").is_none());
}
#[test] fn cwd_template_provenance_survives_resolution(){resolved_template("cwd");}
#[test] fn session_template_provenance_survives_resolution(){resolved_template("session");}
#[test] fn project_cwd_template_provenance_survives_resolution(){resolved_template("PROJECT_CWD");}
#[test]
fn identity_tracks_credentials_and_agent_directory_not_owner_policies() {
    let config=normalize_server(serde_json::from_value(json!({"type":"http","url":"http://127.0.0.1/mcp","auth":"bearer","bearerTokenEnv":"QA_TOKEN"})).unwrap());
    let env=BTreeMap::from([("QA_TOKEN".into(),"synthetic-A".into())]);
    let key=shared_mcp_key("fx",&config,Some(&env),"/agent-A","/cwd");
    let mut policy=config.clone();policy.idle_timeout_min=Some(5.0);policy.lifecycle=Some(Lifecycle::KeepAlive);policy.exposure=Some(Exposure::Search);
    assert_eq!(shared_mcp_key("fx",&policy,Some(&env),"/agent-A","/cwd"),key);
    assert_ne!(shared_mcp_key("fx",&config,Some(&env),"/agent-B","/cwd"),key);
    let different=BTreeMap::from([("QA_TOKEN".into(),"synthetic-B".into())]);
    assert_ne!(shared_mcp_key("fx",&config,Some(&different),"/agent-A","/cwd"),key);
}
