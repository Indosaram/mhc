use maho_ext_api::{Extension, ExtensionApi, ExtensionSessionProfile, SourceInfo};
use maho_ext_host::loader::{NativeExtensionFactory, load_extensions};

struct PatchExtension;
impl Extension for PatchExtension {
    fn register(&self, api: &mut ExtensionApi) {
        maho_ext_gpt_apply_patch::extension::register_apply_patch_extension(api);
    }
}

#[test]
fn native_factory_registers_executable_patch_tool_and_lifecycle() {
    // Given a native factory loaded through the production loader.
    let directory = tempfile::tempdir().unwrap();
    let loaded = load_extensions(vec![NativeExtensionFactory {
        path: "builtin:gpt-apply-patch".into(),
        source_info: SourceInfo { source: "builtin".into(), ..Default::default() },
        extension: Box::new(PatchExtension),
    }], directory.path(), ExtensionSessionProfile::default());
    // When registration finishes, the tool and model lifecycle must exist.
    assert!(loaded.errors.is_empty());
    let extension = &loaded.extensions[0];
    // Then the actual registered definition, not a standalone helper, is executable.
    assert!(extension.tools.iter().any(|tool| tool.definition.name == "apply_patch"));
    assert!(extension.handlers.contains_key(&maho_ext_api::EventKind::SessionStart));
    assert!(extension.handlers.contains_key(&maho_ext_api::EventKind::ModelSelect));
    assert_eq!(extension.lazy_tool_activators.len(), 1);
}

struct SessionPatchExtension;
impl Extension for SessionPatchExtension {
    fn register(&self, api: &mut ExtensionApi) {
        maho_ext_gpt_apply_patch::extension::register_apply_patch_extension(api);
        let runtime = api.runtime.clone();
        api.on(maho_ext_api::EventKind::BeforeAgentStart, std::sync::Arc::new(move |_, _| {
            let runtime = runtime.clone();
            Box::pin(async move {
                let actions = runtime.session_actions()?;
                let mut active = actions.get_active_tools()?;
                active.push("apply_patch".into());
                actions.set_active_tools(active)?;
                Ok(maho_ext_api::EventResult::None)
            })
        }));
    }
}

#[tokio::test]
async fn faux_session_executes_registered_patch_and_records_progress_and_failures() {
    use maho_ai::providers::faux::{faux_assistant_message, faux_tool_call, FauxAssistantMessageOptions};
    use maho_test_support::{faux::FauxScript, faux_session::FauxSession};
    use serde_json::json;
    for partial in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("created.txt");
        let missing = directory.path().join("missing.txt");
        let patch = format!("*** Begin Patch\n*** Add File: {}\n+native\n{}*** End Patch",
            path.display(), if partial { format!("*** Update File: {}\n@@\n-old\n+new\n", missing.display()) } else { String::new() });
        let session = FauxSession::new(FauxScript { name: "registered-patch".into(), prompt: "apply".into(), responses: vec![] })
            .with_native_extension(NativeExtensionFactory {
                path: "builtin:gpt-apply-patch".into(),
                source_info: SourceInfo { source: "builtin".into(), ..Default::default() },
                extension: Box::new(SessionPatchExtension),
            })
            .with_native_responses(vec![
                faux_assistant_message(faux_tool_call("apply_patch", serde_json::from_value(json!({"input":patch})).unwrap(), Some("native-call")),
                    FauxAssistantMessageOptions { stop_reason: Some(maho_ai::types::StopReason::ToolUse), timestamp: Some(0), ..Default::default() }),
                faux_assistant_message("done", FauxAssistantMessageOptions { timestamp: Some(0), ..Default::default() }),
            ]);
        let result = tokio::time::timeout(std::time::Duration::from_secs(10), session.run_native()).await.unwrap().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"native\n");
        let tool = result["messages"].as_array().unwrap().iter().find(|message| message["role"] == "toolResult").unwrap();
        assert_eq!(tool["isError"], partial);
        assert_eq!(tool["details"]["result"]["appliedFiles"], json!([path.to_string_lossy()]));
        assert_eq!(tool["details"]["result"]["failures"].as_array().unwrap().len(), usize::from(partial));
        assert!(result["events"].as_array().unwrap().iter().any(|event| event["type"] == "tool_execution_update"));
        println!("native session: partial={partial}; bytes=native\\n; isError={}; progress=present; cleanup=RAII", tool["isError"]);
    }
}
