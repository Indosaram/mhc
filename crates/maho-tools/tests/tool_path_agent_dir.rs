#[cfg(not(windows))]
#[tokio::test]
async fn tool_path_leaves_agent_dir_alone_and_adds_no_scope_variable() {
    use maho_tools::definition::{AbortSignal, ToolCall, ToolContent};
    use maho_tools::index::{ToolsOptions, create_all_tool_definitions};
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().expect("tempdir");
    let agent_dir = temp.path().join("agent");
    let cwd = temp.path().join("cwd");
    std::fs::create_dir_all(&agent_dir).expect("mkdir agent");
    std::fs::create_dir_all(&cwd).expect("mkdir cwd");

    std::fs::set_permissions(&agent_dir, std::fs::Permissions::from_mode(0o555))
        .expect("chmod 555");

    let tools = create_all_tool_definitions(&cwd, ToolsOptions::default());
    let bash = &tools["bash"];

    let res = (bash.execute)(ToolCall {
        id: "b1",
        params: json!({"command": "echo hello"}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("bash echo");
    let text = match &res.content[0] {
        ToolContent::Text { text, .. } => text.trim().to_owned(),
        _ => panic!("expected text"),
    };
    assert_eq!(text, "hello");

    let res_env = (bash.execute)(ToolCall {
        id: "b2",
        params: json!({"command": "env; echo END"}),
        signal: AbortSignal::default(),
        on_update: None,
        context: None,
    })
    .await
    .expect("bash env");
    let env_text = match &res_env.content[0] {
        ToolContent::Text { text, .. } => text.clone(),
        _ => panic!("expected text"),
    };
    assert!(env_text.contains("END"));
    assert!(!env_text.contains("MACHINE_SCOPE="));

    std::fs::set_permissions(&agent_dir, std::fs::Permissions::from_mode(0o755))
        .expect("restore 755");
    assert_eq!(std::fs::read_dir(&agent_dir).expect("readdir").count(), 0);
}
