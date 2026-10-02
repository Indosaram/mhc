use maho_ext_api::types::*;
use maho_test_support::{faux::{FauxScript,FauxResponse},faux_session::FauxSession};
use serde_json::json;
use std::sync::Arc;

struct ConfiguredHooks;
impl Extension for ConfiguredHooks {
    fn register(&self,api:&mut ExtensionApi) {
        api.on(EventKind::SessionStart,Arc::new(|_,ctx|Box::pin(async move {
            let path=ctx.cwd.join("hooks.json");
            let config=json!({"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"printf '{\"additionalContext\":\"hooks-faux-context\"}'"}]}]}});
            std::fs::write(&path,config.to_string()).map_err(|error|ExtensionFailure::new(error.to_string()))?;
            let source=maho_ext_hooks::types::HookSourceMetadata {scope:maho_ext_hooks::types::HookSourceScope::Global,source_path:path.to_string_lossy().into_owned(),display_order:0,discovered_at:maho_ext_hooks::types::HookDiscoveryTiming::PreSession,plugin_root:None,manifest_path:None,plugin_env:None};
            let parsed=maho_ext_hooks::schema::parse_hook_config(&config,&source);
            let mut trust=maho_ext_hooks::trust_state_json::empty_hook_trust_state();
            for handler in parsed.executable_handlers {trust.hooks.insert(maho_ext_hooks::trust::hook_trust_id(&handler),maho_ext_hooks::trust::create_hook_trust_entry(&handler,"linux","fixed").map_err(|error|ExtensionFailure::new(error.to_string()))?);}
            std::fs::write(ctx.cwd.join("hooks-state.json"),serde_json::to_vec(&trust).map_err(|error|ExtensionFailure::new(error.to_string()))?).map_err(|error|ExtensionFailure::new(error.to_string()))?;
            Ok(EventResult::None)
        })));
        maho_ext_hooks::HooksExtension.register(api);
    }
}

#[tokio::test]
async fn faux_hooks_runs_configured_prompt_command_and_records_context()->Result<(),Box<dyn std::error::Error+Send+Sync>> {
    let script=FauxScript {name:"hooks".to_owned(),prompt:"Run prompt hooks.".to_owned(),responses:vec![FauxResponse {content:"finished".to_owned(),stop_reason:"stop".to_owned()}]};
    let result=FauxSession::new(script).with_native_extension(maho_ext_host::loader::NativeExtensionFactory {path:"<builtin:hooks>".to_owned(),source_info:SourceInfo {source:"builtin".to_owned(),..Default::default()},extension:Box::new(ConfiguredHooks)}).run_native().await?;
    let messages=result["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message|message["role"]=="custom"&&message.to_string().contains("hooks-faux-context")),"native transcript: {result}");
    assert_eq!(messages.last().unwrap()["role"],"assistant");Ok(())
}
