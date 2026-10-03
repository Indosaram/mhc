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

struct PlainTerminal;
impl maho_ext_api::types::Extension for PlainTerminal {
    fn register(&self,api:&mut maho_ext_api::types::ExtensionApi) {maho_ext_terminal::extension::TerminalExtension.register(api);}
}

fn factory(extension:Box<dyn maho_ext_api::types::Extension>)->maho_ext_host::loader::NativeExtensionFactory {
    maho_ext_host::loader::NativeExtensionFactory {path:"<builtin:terminal>".to_owned(),source_info:maho_ext_api::types::SourceInfo {source:"builtin".to_owned(),..Default::default()},extension}
}

fn script(name:&str)->FauxScript {FauxScript {name:name.to_owned(),prompt:"Run the terminal command.".to_owned(),responses:vec![FauxResponse {content:"finished".to_owned(),stop_reason:"stop".to_owned()}]}}

fn assistant_call(name:&str,arguments:serde_json::Map<String,serde_json::Value>,call_id:&str)->maho_ai::types::AssistantMessage {faux_assistant_message(vec![faux_tool_call(name,arguments,Some(call_id))],FauxAssistantMessageOptions {stop_reason:Some(StopReason::ToolUse),timestamp:Some(0),..Default::default()})}

fn tool_results(result:&serde_json::Value)->Vec<serde_json::Value> {
    result["messages"].as_array().unwrap().iter().filter(|message|message["role"]=="toolResult").cloned().collect()
}

#[tokio::test]
async fn faux_terminal_executes_registered_tool_and_returns_real_output()->Result<(),Box<dyn std::error::Error+Send+Sync>> {
    let call=faux_tool_call("bash",json!({"command":"stty size; printf 'terminal-faux-marker:%s' \"${BASH_VERSION:+bash}\"","timeout":5}).as_object().unwrap().clone(),Some("terminal-call"));
    let responses=vec![faux_assistant_message(vec![call],FauxAssistantMessageOptions {stop_reason:Some(StopReason::ToolUse),timestamp:Some(0),..Default::default()}),faux_assistant_message("finished",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()})];
    let result=FauxSession::new(script("terminal")).with_native_extension(factory(Box::new(ConfiguredTerminal))).with_native_responses(responses).run_native().await?;
    let tool=tool_results(&result).into_iter().next().expect("native terminal result");
    assert_eq!(tool["toolName"],"bash");assert_ne!(tool["isError"],true);
    assert!(tool["content"].as_array().unwrap().iter().any(|part|part["text"].as_str().is_some_and(|text|text.contains("33 91")&&text.contains("terminal-faux-marker:bash"))),"native transcript: {result}");
    assert_eq!(result["messages"].as_array().unwrap().last().unwrap()["role"],"assistant");Ok(())
}

#[tokio::test]
async fn faux_terminal_background_command_reports_a_running_session_id()->Result<(),Box<dyn std::error::Error+Send+Sync>> {
    let responses=vec![
        assistant_call("bash",json!({"command":"stty -echo; printf 'bg-ready\\n'","run_in_background":true}).as_object().unwrap().clone(),"bg-call"),
        faux_assistant_message("started",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}),
    ];
    let result=FauxSession::new(script("terminal-background")).with_native_extension(factory(Box::new(PlainTerminal))).with_native_responses(responses).run_native().await?;
    let tool=tool_results(&result).into_iter().next().expect("native background result");
    assert_eq!(tool["toolName"],"bash");assert_ne!(tool["isError"],true);
    let text=tool["content"].as_array().unwrap().iter().filter_map(|part|part["text"].as_str()).collect::<String>();
    assert!(text.contains("Command running in background with ID: bash_1"),"native transcript: {result}");
    assert_eq!(tool["details"]["bash_id"],"bash_1");assert_eq!(tool["details"]["background"],true);Ok(())
}

#[tokio::test]
async fn faux_terminal_kill_all_tears_down_the_background_session()->Result<(),Box<dyn std::error::Error+Send+Sync>> {
    let responses=vec![
        assistant_call("bash",json!({"command":"stty -echo; read value","run_in_background":true}).as_object().unwrap().clone(),"bg-call"),
        assistant_call("kill_bash",json!({"all":true}).as_object().unwrap().clone(),"kill-call"),
        faux_assistant_message("stopped",FauxAssistantMessageOptions {timestamp:Some(0),..Default::default()}),
    ];
    let result=FauxSession::new(script("terminal-kill")).with_native_extension(factory(Box::new(PlainTerminal))).with_native_responses(responses).run_native().await?;
    let results=tool_results(&result);assert_eq!(results.len(),2);
    assert_eq!(results[1]["toolName"],"kill_bash");assert_ne!(results[1]["isError"],true);
    assert!(results[1]["content"].as_array().unwrap().iter().any(|part|part["text"].as_str().is_some_and(|text|text.contains("Killed"))),"native transcript: {result}");Ok(())
}