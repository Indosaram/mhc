#[tokio::test]
async fn minimal_child_resources_do_not_discover_parent_resources_or_run_factories() {
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    use maho_core::{sdk::{create_agent_session, CreateAgentSessionOptions},
        session_manager::SessionManager, settings_manager::{SettingsManager, InMemorySettingsStorage}};
    let dir = tempfile::tempdir().expect("isolated child resources");
    let cwd = dir.path().to_string_lossy().into_owned();
    let prompt = dir.path().join("parent-prompt.md");
    let skill = dir.path().join("SKILL.md");
    std::fs::write(&prompt, "---\nname: parent-prompt\ndescription: fixture\n---\nPARENT_PROMPT").expect("parent prompt");
    std::fs::write(&skill, "---\nname: parent-skill\ndescription: fixture\n---\nPARENT_SKILL").expect("parent skill");
    std::fs::write(dir.path().join("AGENTS.md"), "PARENT_CONTEXT").expect("parent context");
    let mut settings = SettingsManager::from_storage(Box::<InMemorySettingsStorage>::default(), false);
    settings.apply_overrides(&serde_json::Map::from_iter([
        ("prompts".into(), serde_json::json!([prompt])),
        ("skills".into(), serde_json::json!([skill])),
    ]));
    let calls = Arc::new(AtomicUsize::new(0));
    let observed = calls.clone();
    let factory = maho_ext_host::loader::NativeAsyncExtensionFactory {
        path: "parent-factory".into(), source_info: Default::default(),
        factory: Arc::new(move |_| {
            observed.fetch_add(1, Ordering::SeqCst);
            Box::pin(async { Ok(()) })
        }),
    };
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let tool = maho_ext_api::ToolDefinition::new("child_fixture", "child tool",
        serde_json::json!({"type":"object","properties":{}}), Arc::new(|call| {
            let id = call.id.to_owned();
            Box::pin(async move { Ok(maho_ext_api::ToolResult::text(id)) })
        }));

    let created = tokio::time::timeout(std::time::Duration::from_secs(5), create_agent_session(CreateAgentSessionOptions {
        cwd: Some(cwd.clone()), agent_dir: Some(dir.path().join("agent").to_string_lossy().into_owned()),
        model: Some(provider.get_model(Some("faux-1")).expect("model")),
        session_manager: Some(SessionManager::in_memory(&cwd, None, None)), settings_manager: Some(settings),
        tools: Some(vec!["child_fixture".into()]), custom_tools: vec![tool],
        minimal_resources: true, extension_factories: vec![factory],
        system_prompt: Some("PARENT_SYSTEM".into()), append_system_prompt: vec!["PARENT_APPEND".into()],
        ..Default::default()
    })).await.expect("bounded construction").expect("minimal native SDK session");
    let options = created.session.extension_context_actions().get_system_prompt_options();
    let templates = created.session.prompt_templates();
    let bound = created.session.extension_runner_bound().await;
    let result = created.session.execute_tool_with_call_id("child-resource-call", "child_fixture",
        serde_json::json!({}), Default::default()).await;
    created.session.dispose().await;

    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(!bound);
    assert!(templates.is_empty());
    assert!(options.skills.is_empty());
    assert!(options.context_files.is_empty());
    assert!(options.custom_prompt.is_none());
    assert!(options.append_system_prompt.is_none());
    assert_eq!(maho_ai::utils::text::content_text(&result.expect("injected child tool").content, ""), "child-resource-call");
}
