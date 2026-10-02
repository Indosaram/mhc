use maho_ai::providers::faux::{faux_assistant_message,faux_tool_call,FauxAssistantMessageOptions};
use maho_ai::types::StopReason;
use maho_test_support::{faux::{FauxScript,FauxResponse},faux_session::FauxSession};
use serde_json::json;

struct ConfiguredTerminal;
impl maho_ext_api::types::Extension for ConfiguredTerminal {
    fn register(&self,api:&mut maho_ext_api::types::ExtensionApi) {
        api.on(maho_ext_api::types::EventKind::SessionStart,std::sync::Arc::new(|_,ctx|Box::pin(async move {
            std::fs::create_dir_all(&ctx.agent_dir).map_err(|error|maho_ext_api::types::ExtensionFailure::new(error.to_string()))?;
            std::fs::write(ctx.agent_dir.join("settings.json"),json!({"terminal":{"defaultCols":91,"defaultRows":33,"notify":"off"},"shellPath":"/bin/bash"}).to_string()).map_err(|error|maho_ext_api::types::ExtensionFailure::new(error.to_string()))?;
            Ok(maho_ext_api::types::EventResult::None)
        })));
        maho_ext_terminal::extension::TerminalExtension.register(api);
    }
}

#[tokio::test]
async fn faux_terminal_executes_registered_tool_and_returns_real_output()->Result<(),Box<dyn std::error::Error+Send+Sync>> {
    let script=FauxScript {name:"terminal".to_owned(),prompt:"Run the terminal command.".to_owned(),responses:vec![FauxResponse {content:"finished".to_owned(),stop_reason:"stop".to_owned()}]};
    let call=faux_tool_call("bash",json!({"command":"stty size; printf 'terminal-faux-marker:%s' \"${BASH_VERSION:+bash}\"","timeout":5}).as_object().unwrap().clone(),Some("terminal-call"));
    let responses=vec![faux_assistant_message(vec![call],FauxAssistantMessageOptions {stop_reason:Some(StopReason::ToolUse),timestamp:Some(0),..Default::default()}),faux_assistant_message("finished",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()})];
    let result=FauxSession::new(script).with_native_extension(maho_ext_host::loader::NativeExtensionFactory {path:"<builtin:terminal>".to_owned(),source_info:maho_ext_api::types::SourceInfo {source:"builtin".to_owned(),..Default::default()},extension:Box::new(ConfiguredTerminal)}).with_native_responses(responses).run_native().await?;
    let messages=result["messages"].as_array().unwrap();
    let tool=messages.iter().find(|message|message["role"]=="toolResult").expect("native terminal result");
    assert_eq!(tool["toolName"],"bash");assert_ne!(tool["isError"],true);
    assert!(tool["content"].as_array().unwrap().iter().any(|part|part["text"].as_str().is_some_and(|text|text.contains("33 91")&&text.contains("terminal-faux-marker:bash"))),"native transcript: {result}");
    assert_eq!(messages.last().unwrap()["role"],"assistant");Ok(())
}
