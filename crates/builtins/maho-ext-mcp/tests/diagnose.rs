use std::{sync::{Arc,Mutex},time::Duration,collections::BTreeMap};
use maho_ext_mcp::{config_schema::*,connection::*,log::McpLogger,health::with_mcp_session_expiry_retry,errors::{McpError,McpErrorKind}};
fn config()->McpServerConfig {McpServerConfig {enabled:Some(true),transport:Some(Transport::Stdio),command:Some("/usr/bin/node".into()),args:Some(vec!["/home/indo/code/senpi/packages/coding-agent/test/mcp/fixtures/stdio-server.ts".into()]),..Default::default()}}
#[tokio::test]
async fn stderr_diagnostics_are_redacted_and_retained() {
    let root=tempfile::tempdir().unwrap();let mut config=config();config.args.as_mut().unwrap().extend(["--fatal-missing-token".into(),"FOO_TOKEN=synthetic-secret".into()]);
    let connection=ServerConnection::new("fatal",config,None,Arc::new(Mutex::new(McpLogger::new("fatal",root.path(),None).unwrap())));
    let error=connection.connect().await.err().expect("fatal fixture fails");
    assert!(error.message.contains("FATAL: missing FOO_TOKEN=<redacted:"));assert!(!error.message.contains("synthetic-secret"));
    assert_eq!(connection.last_error().expect("last error").message,error.message);
    connection.dispose().await.unwrap();
}
#[tokio::test]
async fn missing_command_diagnostic_reports_spawn_environment() {
    let root=tempfile::tempdir().unwrap();let mut config=config();config.command=Some("definitely-not-a-real-mcp-command".into());config.cwd=Some(root.path().display().to_string());
    let connection=ServerConnection::new("missing",config,Some(BTreeMap::from([("PATH".into(),"/tmp/mcp-fixture-bin".into())])),Arc::new(Mutex::new(McpLogger::new("missing",root.path(),None).unwrap())));
    let error=connection.connect().await.err().expect("missing command fails");
    assert!(error.message.contains("command not found: definitely-not-a-real-mcp-command"));assert!(error.message.contains("PATH: /tmp/mcp-fixture-bin"));assert!(error.message.contains(root.path().to_str().unwrap()));
    connection.dispose().await.unwrap();
}
#[tokio::test]
async fn second_session_expiry_suspends_after_exactly_one_renewal() {
    let root=tempfile::tempdir().unwrap();let connection=ServerConnection::new("expiry",config(),None,Arc::new(Mutex::new(McpLogger::new("expiry",root.path(),None).unwrap())));
    connection.connect().await.unwrap();let mut attempts=0;
    let result=with_mcp_session_expiry_retry(&connection,||{attempts+=1;async {Err::<(),_>(McpError::new(McpErrorKind::SessionExpired,"MCP error -32001: session expired"))}}).await;
    assert_eq!(attempts,2);assert_eq!(result.unwrap_err().kind,McpErrorKind::SessionExpired);assert_eq!(connection.state(),ServerConnectionState::Suspended);assert_eq!(connection.generation(),1);
    tokio::time::timeout(Duration::from_secs(5),connection.dispose()).await.unwrap().unwrap();
}
