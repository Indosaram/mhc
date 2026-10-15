use maho_ai::types::{AssistantMessage, ContentBlock, StopReason};
use maho_ai::utils::provider_failure_description::{describe_provider_failure_for_user,strip_turn_retry_suppression_prefix};
use maho_ai::utils::retry::ProviderStallDescriptionOptions;
use crate::provider_native_rendering::{format_provider_native_body,format_provider_native_summary};

#[derive(Default,Debug,PartialEq)]
pub struct PrintOutput { pub stdout:String,pub stderr:String,pub exit_code:i32 }
pub async fn run_print_runtime(
    runtime:&maho_core::agent_session_runtime::AgentSessionRuntime,
    scope:&maho_ai::node::provider_scope::ProviderScope,
    json_mode:bool,
    initial:Option<(String,Vec<maho_ai::types::ImageContent>)>,
    messages:&[String],
    output:impl tokio::io::AsyncWrite+Unpin,
    diagnostics:impl tokio::io::AsyncWrite+Unpin,
)->std::io::Result<i32>{
    let result=maho_ai::node::provider_scope::run_with_provider_scope_async(scope,run_print_session(runtime.session(),json_mode,initial,messages,output,diagnostics)).await.map_err(std::io::Error::other);
    let disposed=crate::session_teardown::dispose_runtime(runtime,scope).await.map_err(std::io::Error::other);
    let result=result?;
    disposed?;
    result
}
pub async fn run_print_session(
    session:&maho_core::agent_session::AgentSession,
    json_mode:bool,
    initial:Option<(String,Vec<maho_ai::types::ImageContent>)>,
    messages:&[String],
    mut output:impl tokio::io::AsyncWrite+Unpin,
    mut diagnostics:impl tokio::io::AsyncWrite+Unpin,
)->std::io::Result<i32>{
    use tokio::io::AsyncWriteExt;
    let(sender,mut events)=tokio::sync::mpsc::unbounded_channel();
    let event_session=session.weak_accessor();
    let _subscription=session.subscribe(std::sync::Arc::new(move|event|{
        // Text mode never reads the wire record, so skip serializing it.
        if !json_mode{let _=sender.send((fallback_diagnostic(event),Ok(serde_json::Value::Null)));return;}
        let mut record=match event{
            maho_ext_api::AgentSessionEvent::Agent(maho_ext_api::AgentEvent::MessageUpdate{message,assistant_message_event})=>stripped_message_update(message,assistant_message_event).unwrap_or_else(||crate::session_binding::session_event_record(event)),
            _=>crate::session_binding::session_event_record(event),
        };
        // senpi publishes `message_update` with `resolvedToolName` (core/tool-call-display-name.ts).
        if let(Ok(record),maho_ext_api::AgentSessionEvent::Agent(update@maho_ext_api::AgentEvent::MessageUpdate{..}))=(&mut record,event)
            &&let Some(name)=event_session().and_then(|session|session.message_update_resolved_tool_name(update)){record["resolvedToolName"]=name.into();}
        let _=sender.send((fallback_diagnostic(event),record));
    }));
    if json_mode&&let Some(header)=session.with_session_manager(|manager|manager.header()){
        output.write_all(crate::jsonl::serialize_json_line(&header)?.as_bytes()).await?;
    }
    // A turn only runs when a prompt was supplied; with none (an empty/`--no-...` invocation) the
    // session never settles a turn, so no `agent_idle` is expected and the runner must not wait.
    let ran_prompt=initial.is_some()||!messages.is_empty();
    let prompts=async{
        if let Some((text,images))=initial{session.prompt(&text,maho_core::agent_session::PromptOptions{images:Some(images),session_title_prompt:Some(maho_core::agent_session::SessionTitlePrompt::Disabled),..Default::default()}).await?;}
        for text in messages{session.prompt(text,maho_core::agent_session::PromptOptions{session_title_prompt:Some(maho_core::agent_session::SessionTitlePrompt::Disabled),..Default::default()}).await?;}
        session.wait_for_idle().await;Ok::<(),String>(())
    };
    tokio::pin!(prompts);
    let mut agent_idle_seen=false;
    let result=loop{
        tokio::select!{
            result=&mut prompts=>break result,
            event=events.recv()=>if let Some((diagnostic,event))=event{
                if let Some(diagnostic)=diagnostic{diagnostics.write_all(diagnostic.as_bytes()).await?;}
                if json_mode{let event=event.map_err(std::io::Error::other)?;let event=crate::json_event::to_json_event(&event).map_err(std::io::Error::other)?;if event["type"]=="agent_idle"{agent_idle_seen=true;}output.write_all(crate::jsonl::serialize_json_line(&event)?.as_bytes()).await?;}
            }
        }
    };
    // senpi's print mode finishes after `waitForSettledSessionWork`, which covers the settlement-
    // deferred `agent_idle` emission. That record is published by a task spawned during settlement,
    // so it can arrive just after the prompts future settles; await that exact record (bounded)
    // before exiting so the JSON stream carries the same terminal event as the pinned runner.
    if json_mode&&ran_prompt&&result.is_ok()&&!agent_idle_seen{
        let deadline=tokio::time::Instant::now()+std::time::Duration::from_secs(5);
        loop{
            let remaining=deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero(){break;}
            match tokio::time::timeout(remaining,events.recv()).await{
                Ok(Some((diagnostic,event))) => {
                    if let Some(diagnostic)=diagnostic{diagnostics.write_all(diagnostic.as_bytes()).await?;}
                    let event=event.map_err(std::io::Error::other)?;let event=crate::json_event::to_json_event(&event).map_err(std::io::Error::other)?;
                    let idle=event["type"]=="agent_idle";
                    output.write_all(crate::jsonl::serialize_json_line(&event)?.as_bytes()).await?;
                    if idle{break;}
                }
                Ok(None)|Err(_)=>break,
            }
        }
    }
    while let Ok((diagnostic,event))=events.try_recv(){
        if let Some(diagnostic)=diagnostic{diagnostics.write_all(diagnostic.as_bytes()).await?;}
        if json_mode{let event=event.map_err(std::io::Error::other)?;let event=crate::json_event::to_json_event(&event).map_err(std::io::Error::other)?;output.write_all(crate::jsonl::serialize_json_line(&event)?.as_bytes()).await?;}
    }
    let exit_code=match result{
        Err(error)=>{diagnostics.write_all(format!("{error}\n").as_bytes()).await?;1},
        Ok(()) if !json_mode=>{
            let messages=session.messages();
            let assistant=messages.iter().rev().find_map(|message|message.as_assistant());
            let rendered=format_print_result(assistant).map_err(std::io::Error::other)?;
            output.write_all(rendered.stdout.as_bytes()).await?;diagnostics.write_all(rendered.stderr.as_bytes()).await?;rendered.exit_code
        },
        Ok(())=>0
    };
    output.flush().await?;diagnostics.flush().await?;Ok(exit_code)
}
/// The `message_update` record `to_json_event` reads, without the cumulative `message` and
/// `partial` snapshots it discards (`partial` stays a trailing null so key order is unchanged).
/// `None` when the full record is needed: a non-assistant message, `toolcall_start` (reads
/// `partial`), or the terminal `done`/`error` events.
fn stripped_message_update(message:&maho_ext_api::AgentMessage,event:&maho_ai::types::AssistantMessageEvent)->Option<Result<serde_json::Value,serde_json::Error>>{
    use maho_ai::types::AssistantMessageEvent as A;
    use serde_json::json;
    let assistant=message.as_assistant()?;
    let event=match event{
        A::Start{..}=>json!({"type":"start","partial":null}),
        A::TextStart{content_index,..}=>json!({"type":"text_start","contentIndex":content_index,"partial":null}),
        A::TextDelta{content_index,delta,..}=>json!({"type":"text_delta","contentIndex":content_index,"delta":delta,"partial":null}),
        A::TextEnd{content_index,content,..}=>json!({"type":"text_end","contentIndex":content_index,"content":content,"partial":null}),
        A::ThinkingStart{content_index,..}=>json!({"type":"thinking_start","contentIndex":content_index,"partial":null}),
        A::ThinkingDelta{content_index,delta,..}=>json!({"type":"thinking_delta","contentIndex":content_index,"delta":delta,"partial":null}),
        A::ThinkingEnd{content_index,content,..}=>json!({"type":"thinking_end","contentIndex":content_index,"content":content,"partial":null}),
        A::ToolcallDelta{content_index,delta,..}=>json!({"type":"toolcall_delta","contentIndex":content_index,"delta":delta,"partial":null}),
        A::ToolcallEnd{content_index,tool_call,..}=>match serde_json::to_value(tool_call){Ok(tool_call)=>json!({"type":"toolcall_end","contentIndex":content_index,"toolCall":tool_call,"partial":null}),Err(error)=>return Some(Err(error))},
        A::ToolcallStart{..}|A::Done{..}|A::Error{..}=>return None,
    };
    Some(serde_json::to_value(assistant.usage).map(|usage|json!({"type":"message_update","message":{"role":"assistant","usage":usage},"assistantMessageEvent":event})))
}
pub fn fallback_diagnostic(event:&maho_ext_api::AgentSessionEvent)->Option<String>{use maho_ext_api::AgentSessionEvent;match event{
    AgentSessionEvent::RetryFallbackApplied{from,to,reason,..}=>Some(format!("Model fallback: {from} -> {to} ({reason})\n")),
    AgentSessionEvent::RetryFallbackExhausted{chain_key,last_error}=>Some(format!("Model fallback exhausted: {chain_key} ({last_error})\n")),
    AgentSessionEvent::RetryFallbackReverted{from,to}=>Some(format!("Model fallback reverted: {from} -> {to}\n")),
    _=>None,
}}
pub fn format_print_result(message: Option<&AssistantMessage>) -> Result<PrintOutput,serde_json::Error> {
    let Some(message) = message else { return Ok(PrintOutput::default()); };
    if matches!(message.stop_reason,StopReason::Error|StopReason::Aborted) {
        let error = describe_provider_failure_for_user(message.error_message.as_deref(),&ProviderStallDescriptionOptions::default())
            .unwrap_or_else(|| {
                let stripped = strip_turn_retry_suppression_prefix(message.error_message.as_deref().unwrap_or(""));
                if stripped.is_empty() { format!("Request {}",message.stop_reason.as_str()) } else { stripped }
            });
        return Ok(PrintOutput { stdout:String::new(),stderr:format!("{error}\n"),exit_code:1 });
    }
    let mut output = PrintOutput::default();
    for content in &message.content {
        match content {
            ContentBlock::Text(text) => { output.stdout.push_str(&text.text);output.stdout.push('\n'); }
            ContentBlock::ProviderNative(content) => {
                output.stdout.push_str(&format_provider_native_summary(message.provider.as_str(),&content.subtype,&content.raw,false));output.stdout.push('\n');
                output.stdout.push_str(&format_provider_native_body(&content.subtype,&content.raw,false)?);output.stdout.push('\n');
            }
            ContentBlock::Thinking(_) | ContentBlock::ToolCall(_) | ContentBlock::Image(_) => {}
        }
    }
    Ok(output)
}
/// Options for print mode (senpi `PrintModeOptions`).
#[derive(Debug,Clone,Default)]
pub struct PrintModeOptions{pub json_mode:bool,pub messages:Vec<String>,pub initial_message:Option<String>,pub initial_images:Vec<maho_ai::types::ImageContent>}
/// Run print (single-shot) mode: send the prompts, output the result, return the exit code
/// (senpi `runPrintMode`).
pub async fn run_print_mode(runtime:&maho_core::agent_session_runtime::AgentSessionRuntime,scope:&maho_ai::node::provider_scope::ProviderScope,options:PrintModeOptions,output:impl tokio::io::AsyncWrite+Unpin,diagnostics:impl tokio::io::AsyncWrite+Unpin)->std::io::Result<i32>{
    let initial=options.initial_message.map(|text|(text,options.initial_images.clone()));
    run_print_runtime(runtime,scope,options.json_mode,initial,&options.messages,output,diagnostics).await
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn message(content:serde_json::Value,stop:&str,error:Option<&str>) -> AssistantMessage {
        serde_json::from_value(json!({"role":"assistant","content":content,"api":"openai-completions","provider":"faux","model":"faux-1","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":stop,"errorMessage":error,"timestamp":0})).unwrap()
    }
    #[test] fn prints_text_blocks_with_newlines() { let message=message(json!([{"type":"text","text":"one"},{"type":"text","text":"two"}]),"stop",None);assert_eq!(format_print_result(Some(&message)).unwrap().stdout,"one\ntwo\n"); }
    #[test] fn provider_native_uses_summary_and_body() { let message=message(json!([{"type":"providerNative","subtype":"image_generation_call","raw":{"result":"YWJj"}}]),"stop",None);let output=format_print_result(Some(&message)).unwrap();assert!(output.stdout.ends_with("\n3 bytes\n"));assert!(!output.stdout.contains("YWJj")); }
    #[test] fn failed_turn_strips_internal_marker_and_exits_one() { let message=message(json!([]),"error",Some("senpi:no-turn-retry:failed"));assert_eq!(format_print_result(Some(&message)).unwrap(),PrintOutput { stdout:String::new(),stderr:"failed\n".into(),exit_code:1 }); }
    #[test] fn missing_error_uses_terminal_status() { let message=message(json!([]),"aborted",None);assert_eq!(format_print_result(Some(&message)).unwrap().stderr,"Request aborted\n"); }
    #[test] fn stripped_message_update_matches_full_record_json_event() {
        let partial=message(json!([{"type":"text","text":"ab"},{"type":"toolCall","id":"c1","name":"bash","arguments":{"x":1}}]),"stop",None);
        let agent_message=maho_ext_api::AgentMessage::Llm(maho_ai::types::Message::Assistant(Box::new(partial.clone())));
        let tool_call:maho_ai::types::ToolCall=serde_json::from_value(json!({"id":"c1","name":"bash","arguments":{"x":1}})).unwrap();
        use maho_ai::types::AssistantMessageEvent as A;
        let events=vec![A::Start{partial:partial.clone()},A::TextStart{content_index:0,partial:partial.clone()},A::TextDelta{content_index:0,delta:"b".into(),partial:partial.clone()},A::TextEnd{content_index:0,content:"ab".into(),partial:partial.clone()},A::ThinkingStart{content_index:0,partial:partial.clone()},A::ThinkingDelta{content_index:0,delta:"t".into(),partial:partial.clone()},A::ThinkingEnd{content_index:0,content:"t".into(),partial:partial.clone()},A::ToolcallDelta{content_index:1,delta:"{".into(),partial:partial.clone()},A::ToolcallEnd{content_index:1,tool_call,partial:partial.clone()}];
        for assistant_message_event in events {
            let event=maho_ext_api::AgentSessionEvent::Agent(maho_ext_api::AgentEvent::MessageUpdate{message:agent_message.clone(),assistant_message_event:assistant_message_event.clone()});
            let full=crate::session_binding::session_event_record(&event).unwrap();
            let stripped=stripped_message_update(&agent_message,&assistant_message_event).unwrap().unwrap();
            let expected=crate::jsonl::serialize_json_line(&crate::json_event::to_json_event(&full).unwrap()).unwrap();
            assert_eq!(crate::jsonl::serialize_json_line(&crate::json_event::to_json_event(&stripped).unwrap()).unwrap(),expected);
        }
        assert!(stripped_message_update(&agent_message,&A::ToolcallStart{content_index:1,partial}).is_none());
    }
    #[test] fn absent_assistant_outputs_nothing() { assert_eq!(format_print_result(None).unwrap(),PrintOutput::default()); }
}
