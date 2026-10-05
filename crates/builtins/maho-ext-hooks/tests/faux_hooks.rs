use maho_ext_api::types::*;
use maho_test_support::{faux::{FauxScript,FauxResponse},faux_session::FauxSession};
use serde_json::json;
use std::sync::Arc;

fn install_hooks(ctx:&ExtensionContext,config:serde_json::Value)->Result<(),ExtensionFailure> {
    let path=ctx.cwd.join("hooks.json");
    std::fs::write(&path,config.to_string()).map_err(|error|ExtensionFailure::new(error.to_string()))?;
    let source=maho_ext_hooks::types::HookSourceMetadata {scope:maho_ext_hooks::types::HookSourceScope::Global,source_path:path.to_string_lossy().into_owned(),display_order:0,discovered_at:maho_ext_hooks::types::HookDiscoveryTiming::PreSession,plugin_root:None,manifest_path:None,plugin_env:None};
    let parsed=maho_ext_hooks::schema::parse_hook_config(&config,&source);
    let mut trust=maho_ext_hooks::trust_state_json::empty_hook_trust_state();
    for handler in parsed.executable_handlers {trust.hooks.insert(maho_ext_hooks::trust::hook_trust_id(&handler),maho_ext_hooks::trust::create_hook_trust_entry(&handler,"linux","fixed").map_err(|error|ExtensionFailure::new(error.to_string()))?);}
    std::fs::write(ctx.cwd.join("hooks-state.json"),serde_json::to_vec(&trust).map_err(|error|ExtensionFailure::new(error.to_string()))?).map_err(|error|ExtensionFailure::new(error.to_string()))?;
    Ok(())
}

struct ConfiguredHooks;
impl Extension for ConfiguredHooks {
    fn register(&self,api:&mut ExtensionApi) {
        api.on(EventKind::SessionStart,Arc::new(|_,ctx|Box::pin(async move {
            let config=json!({"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"printf '{\"additionalContext\":\"hooks-faux-context\"}'"}]}]}});
            install_hooks(ctx,config)?;
            Ok(EventResult::None)
        })));
        maho_ext_hooks::HooksExtension.register(api);
    }
}

struct StopHooks;
impl Extension for StopHooks {
    fn register(&self,api:&mut ExtensionApi) {
        api.on(EventKind::SessionStart,Arc::new(|_,ctx|Box::pin(async move {
            let config=json!({"hooks":{"Stop":[{"hooks":[{"type":"command","command":"printf '{\"decision\":\"block\",\"hookSpecificOutput\":{\"hookEventName\":\"Stop\",\"additionalContext\":\"stop-hook-context\"}}'"}]}]}});
            install_hooks(ctx,config)?;
            Ok(EventResult::None)
        })));
        maho_ext_hooks::HooksExtension.register(api);
    }
}

fn factory(extension:Box<dyn Extension>)->maho_ext_host::loader::NativeExtensionFactory {
    maho_ext_host::loader::NativeExtensionFactory {path:"<builtin:hooks>".to_owned(),source_info:SourceInfo {source:"builtin".to_owned(),..Default::default()},extension}
}

fn script(name:&str,prompt:&str)->FauxScript {FauxScript {name:name.to_owned(),prompt:prompt.to_owned(),responses:vec![FauxResponse {content:"finished".to_owned(),stop_reason:"stop".to_owned()}]}}

#[tokio::test]
async fn faux_hooks_runs_configured_prompt_command_and_records_context()->Result<(),Box<dyn std::error::Error+Send+Sync>> {
    let result=FauxSession::new(script("hooks","Run prompt hooks.")).with_native_extension(factory(Box::new(ConfiguredHooks))).run_native().await?;
    let messages=result["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message|message["role"]=="custom"&&message.to_string().contains("hooks-faux-context")),"native transcript: {result}");
    assert_eq!(messages.last().unwrap()["role"],"assistant");Ok(())
}

#[tokio::test]
async fn faux_hooks_stop_command_blocks_and_queues_follow_up_context()->Result<(),Box<dyn std::error::Error+Send+Sync>> {
    let script=FauxScript {name:"hooks-stop".to_owned(),prompt:"Ship it.".to_owned(),responses:vec![FauxResponse {content:"draft".to_owned(),stop_reason:"stop".to_owned()},FauxResponse {content:"revised".to_owned(),stop_reason:"stop".to_owned()}]};
    let result=FauxSession::new(script).with_native_extension(factory(Box::new(StopHooks))).run_native().await?;
    let messages=result["messages"].as_array().unwrap();
    assert!(messages.iter().any(|message|message["role"]=="user"&&message.to_string().contains("stop-hook-context")),"native transcript: {result}");
    assert_eq!(messages.iter().filter(|message|message["role"]=="assistant").map(|message|message.to_string()).filter(|text|text.contains("draft")||text.contains("revised")).count(),2);
    assert!(result["entries"].to_string().contains("senpi.hooks.stop-state"),"native entries: {}",result["entries"]);Ok(())
}