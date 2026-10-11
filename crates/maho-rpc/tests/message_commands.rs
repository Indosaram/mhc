use maho_core::{model_runtime::{CreateModelRuntimeOptions, ModelRuntime}, sdk::{CreateAgentSessionOptions, NoToolsMode, create_agent_session}, session_manager::SessionManager, settings_manager::{InMemorySettingsStorage, SettingsManager}};
use maho_rpc::connection_handler::handle_input_line;
use serde_json::{Value, json};

struct NoopUi;
impl maho_ext_api::ExtensionUi for NoopUi {
    fn select<'a>(&'a self, _: &'a str, _: &'a [String], _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn confirm<'a>(&'a self, _: &'a str, _: &'a str, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, bool> { Box::pin(async { false }) }
    fn input<'a>(&'a self, _: &'a str, _: Option<&'a str>, _: maho_ext_api::ExtensionUiDialogOptions) -> maho_ext_api::UiFuture<'a, Option<String>> { Box::pin(async { None }) }
    fn notify(&self, _: &str, _: maho_ext_api::NotificationType) {}
    fn set_status(&self, _: &str, _: Option<&str>) {}
    fn set_widget(&self, _: &str, _: Option<maho_ext_api::WidgetContent>, _: maho_ext_api::ExtensionWidgetOptions) {}
    fn set_header(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_footer(&self, _: Option<maho_ext_api::ComponentFactory>) {}
    fn set_title(&self, _: &str) {}
    fn paste_to_editor(&self, _: &str) {}
    fn set_editor_text(&self, _: &str) {}
    fn get_editor_text(&self) -> String { String::new() }
    fn custom(&self, _: maho_ext_api::ComponentFactory, _: maho_ext_api::CustomUiOptions) -> maho_ext_api::ExtensionFuture<'_, Value> { Box::pin(async { Ok(Value::Null) }) }
    fn theme(&self) -> maho_ext_api::Theme { Default::default() }
}

async fn command(session: &maho_core::agent_session::AgentSession, value: Value) -> Value {
    serde_json::from_str(&Box::pin(handle_input_line(session, &value.to_string())).await.expect("dispatch").expect("response")).expect("JSON response")
}

#[tokio::test]
async fn message_dispatch_updates_persisted_context_and_acknowledges_control_commands() {
    let temp = tempfile::tempdir().expect("root");
    let cwd = temp.path().to_string_lossy().into_owned();
    let directory = temp.path().join("sessions");
    let manager = SessionManager::create(&cwd, Some(directory.to_str().expect("directory")), None);
    let provider = maho_ai::providers::faux::faux_provider(Default::default());
    let model = provider.get_model(Some("faux-1")).expect("model");
    let runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions { providers: Some(vec![provider.provider.clone()]), ..Default::default() });
    let session = create_agent_session(CreateAgentSessionOptions { cwd: Some(cwd.clone()), agent_dir: Some(cwd), model_runtime: Some(runtime), model: Some(model), session_manager: Some(manager), settings_manager: Some(SettingsManager::from_storage(Box::new(InMemorySettingsStorage::default()), false)), no_tools: Some(NoToolsMode::All), auto_title_sessions: Some(false), ..Default::default() }).await.expect("session").session;
    for content in [json!("first user"), json!([{"type":"text","text":"second user"}])] {
        assert_eq!(command(&session, json!({"type":"append_user_message","content":content})).await["success"], true);
    }
    provider.append_responses(vec![maho_ai::providers::faux::faux_assistant_message(
        "fixture assistant",
        Default::default(),
    )
    .into()]);
    Box::pin(session.continue_session()).await.expect("persist fixture turn");
    assert_eq!(command(&session, json!({"type":"send_custom_message","customType":"notice","content":"custom string","display":true})).await["success"], true);
    assert_eq!(command(&session, json!({"type":"send_custom_message","customType":"notice","content":[{"type":"text","text":"custom blocks"}],"display":false})).await["success"], true);
    let messages = command(&session, json!({"type":"get_messages"})).await;
    let messages = messages["data"]["messages"].as_array().expect("messages");
    assert_eq!(messages.iter().filter(|message| message["role"] == "user").count(), 2);
    assert_eq!(messages.iter().filter(|message| message["customType"] == "notice").count(), 2);
    assert!(messages.iter().any(|message| message["customType"] == "notice" && message["content"] == "custom string"));
    assert!(messages.iter().any(|message| message["customType"] == "notice" && message["content"] == json!([{"type":"text","text":"custom blocks"}])));
    assert_eq!(command(&session, json!({"type":"send_custom_message","customType":"empty-notice","display":false})).await["success"], true);
    assert!(command(&session, json!({"type":"get_messages"})).await["data"]["messages"].as_array().expect("messages").iter().any(|message| message["customType"] == "empty-notice" && message["content"] == json!([])));
    let persisted = std::fs::read_to_string(session.session_file().expect("session path")).expect("persisted");
    assert!(persisted.contains("first user"));
    assert!(persisted.contains("custom string"));
    for kind in ["abort_branch_summary", "check_reload_veto"] {
        assert_eq!(command(&session, json!({"type":kind})).await["success"], true);
    }
    let bash = command(&session, json!({"type":"bash","command":"printf RPC_BASH_OK","excludeFromContext":true})).await;
    assert_eq!(bash["success"], true);
    assert_eq!(bash["data"]["exitCode"], 0, "bash response: {bash}");
    assert_eq!(bash["data"]["output"], "RPC_BASH_OK");
    let mut hook = maho_ext_api::LoadedExtension::new("<inline:rpc-bash>", temp.path().into(), Default::default());
    hook.handlers.insert(maho_ext_api::EventKind::UserBash, vec![std::sync::Arc::new(|_, _| Box::pin(async {
        Ok(maho_ext_api::EventResult::UserBash { operations: None, result: Some(maho_tools::bash_executor::BashResult { output: "RPC_HOOK_OK".into(), exit_code: Some(0), cancelled: false, truncated: false, full_output_path: None }) })
    }))]);
    let context = session.extension_context(std::sync::Arc::new(NoopUi));
    session.set_extension_runner(maho_ext_host::ExtensionRunner::new(vec![hook], Default::default(), Default::default(), context)).await;
    let hook_result = command(&session, json!({"type":"bash","command":"exit 99"})).await;
    assert_eq!(hook_result["data"]["output"], "RPC_HOOK_OK");
    assert_eq!(hook_result["data"]["exitCode"], 0);
    let user_id = session.with_session_manager(|manager| manager.entries().into_iter().find(|entry| entry["message"]["role"] == "user").expect("user entry")["id"].as_str().expect("entry id").to_owned());
    let edited = command(&session, json!({"type":"edit_user_message","entryId":user_id,"text":"edited user"})).await;
    assert_eq!(edited["success"], true);
    assert_eq!(edited["data"]["outcome"], "edited");
    assert!(command(&session, json!({"type":"get_messages"})).await["data"]["messages"].to_string().contains("edited user"));
    let edited_id = edited["data"]["entry"]["id"].as_str().expect("edited entry");
    let empty = command(&session, json!({"type":"edit_user_message","entryId":edited_id,"text":"   "})).await;
    assert_eq!(empty["errorCode"], "empty");
    let stale = command(&session, json!({"type":"edit_user_message","entryId":edited_id,"text":"edited user","expectedLeafId":"stale"})).await;
    assert_eq!(stale["errorCode"], "stale_leaf");
    let invalid = command(&session, json!({"type":"append_user_message","content":42})).await;
    assert_eq!(invalid["success"], false);
    let assistant = session.with_session_manager_mut(|manager| manager.append_message(json!({"role":"assistant","content":[{"type":"text","text":"assistant before"}],"api":"openai-completions","provider":"faux","model":"faux-1","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":0})));
    let assistant_id = assistant["id"].as_str().expect("assistant entry");
    let edited_assistant = command(&session, json!({"type":"edit_assistant_message","entryId":assistant_id,"text":"assistant after"})).await;
    assert_eq!(edited_assistant["data"]["outcome"], "edited");
    assert!(command(&session, json!({"type":"get_messages"})).await["data"]["messages"].to_string().contains("assistant after"));
    let unchanged = command(&session, json!({"type":"edit_assistant_message","entryId":edited_assistant["data"]["entry"]["id"],"text":"assistant after"})).await;
    assert_eq!(unchanged["data"]["outcome"], "unchanged");
    for kind in ["edit_user_message", "edit_assistant_message"] {
        for value in [json!({"type":kind,"entryId":"missing","text":42}), json!({"type":kind,"text":"replacement"})] {
            let invalid = command(&session, value).await;
            assert_eq!(invalid["success"], false);
            assert_eq!(invalid["command"], kind);
        }
        let response = command(&session, json!({"type":kind,"entryId":"missing","text":"replacement"})).await;
        assert_eq!(response["success"], false);
        assert_eq!(response["errorCode"], "not_found");
        assert!(!response["error"].as_str().expect("error").contains("unfinished runtime binding"));
    }
    let selected = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
    let theme_selected = selected.clone();
    session.set_html_theme_resolver(std::sync::Arc::new(move |name| {
        *theme_selected.lock().expect("theme selection") = name.map(str::to_owned);
        Ok((maho_ext_api::Theme::default(), Some((Default::default(), Default::default()))))
    }));
    let html_path = temp.path().join("session.html");
    let exported = command(&session, json!({"type":"export_html","outputPath":html_path,"themeName":"fixture-theme"})).await;
    assert_eq!(exported["success"], true);
    assert_eq!(selected.lock().expect("theme selection").as_deref(), Some("fixture-theme"));
    assert!(html_path.is_file());
    session.set_html_theme_resolver(std::sync::Arc::new(|_| Err("fixture theme unavailable".into())));
    let refused = command(&session, json!({"type":"export_html","outputPath":html_path,"themeName":"missing-theme"})).await;
    assert_eq!(refused["success"], false);
    assert!(refused["error"].as_str().expect("theme error").contains("fixture theme unavailable"));
    session.dispose().await;
}
