mod support;

use std::sync::Arc;

use maho_agent::{
    AgentContext, AgentEvent, AgentLoopConfig, AgentMessage, AgentTool, AgentToolResult, BeforeToolCallContext,
    BeforeToolCallResult, ResolveUnknownToolCall, identity_convert_to_llm,
};
use maho_ai::types::{ContentBlock, Message, StopReason, ToolResultMessage, TextAudience, TextContent};
use maho_ai::utils::abort::AbortSignal;
use serde_json::json;
use support::{
    assistant, collect_events, recording_tool, test_model, text_block, text_of, tool_call, user_message,
};

struct CallOnce {
    result: ToolResultMessage,
    start_tool_names: Vec<String>,
    end_tool_names: Vec<String>,
}

async fn call_once(called_name: &str, tools: Vec<AgentTool>, configure: impl FnOnce(&mut AgentLoopConfig)) -> CallOnce {
    let context = AgentContext { system_prompt: String::new(), messages: Vec::new(), tools: Some(tools) };
    let mut config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    configure(&mut config);
    let messages = vec![
        assistant(vec![tool_call("call-1", called_name, json!({ "city": "Seoul" }))], StopReason::ToolUse),
        assistant(vec![text_block("done")], StopReason::Stop),
    ];
    let stream = maho_agent::agent_loop::agent_loop(
        vec![user_message("call it")],
        context,
        config,
        None,
        Some(support::scripted_stream_fn(messages)),
    );
    let events = collect_events(&stream).await;
    let mut start_tool_names = Vec::new();
    let mut end_tool_names = Vec::new();
    for event in &events {
        match event {
            AgentEvent::ToolExecutionStart { tool_name, .. } => start_tool_names.push(tool_name.clone()),
            AgentEvent::ToolExecutionEnd { tool_name, .. } => end_tool_names.push(tool_name.clone()),
            _ => {}
        }
    }
    let messages = stream.result().await.expect("result");
    let result = messages
        .iter()
        .find_map(|message| match message {
            AgentMessage::Llm(Message::ToolResult(result)) => Some(result.clone()),
            _ => None,
        })
        .expect("expected a tool result");
    CallOnce { result, start_tool_names, end_tool_names }
}

fn expected_correction(requested: &str, resolved: &str) -> ContentBlock {
    ContentBlock::Text(TextContent {
        text: format!(
            "[auto-corrected] no tool is named \"{requested}\"; ran \"{resolved}\". Call tools by their exact listed name."
        ),
        audience: Some(TextAudience::Model),
        text_signature: None,
    })
}

#[tokio::test]
async fn runs_the_unique_active_tool_for_a_gateway_namespaced_recased_name_and_reports_the_correction() {
    let (tool, seen) = recording_tool("lazy_weather");
    let seen_by_hook: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let hook_sink = Arc::clone(&seen_by_hook);

    let outcome = call_once("mcp__686f__LazyWeather", vec![tool], move |config| {
        config.before_tool_call = Some(Arc::new(move |context: BeforeToolCallContext, _signal: Option<AbortSignal>| {
            hook_sink
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(context.tool_call.name.clone());
            Box::pin(async { None::<BeforeToolCallResult> })
        }));
    })
    .await;

    assert_eq!(*seen.lock().unwrap(), vec![json!({ "city": "Seoul" })]);
    assert!(!outcome.result.is_error);
    assert_eq!(outcome.result.tool_name, "lazy_weather");
    assert_eq!(*seen_by_hook.lock().unwrap(), vec!["lazy_weather".to_owned()]);
    assert_eq!(outcome.start_tool_names, vec!["lazy_weather".to_owned()]);
    assert_eq!(outcome.end_tool_names, vec!["lazy_weather".to_owned()]);
    assert_eq!(outcome.result.content[0], expected_correction("mcp__686f__LazyWeather", "lazy_weather"));
    assert!(text_of(&outcome.result).contains("lazy_weather:Seoul"));
}

#[tokio::test]
async fn resolves_a_bare_snake_case_name_that_only_differs_by_the_gateway_namespace() {
    let (tool, seen) = recording_tool("lazy_weather");

    let outcome = call_once("mcp__686f__lazy_weather", vec![tool], |_| {}).await;

    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(outcome.result.tool_name, "lazy_weather");
    assert!(!outcome.result.is_error);
}

#[tokio::test]
async fn never_guesses_between_two_tools_that_fold_to_the_same_key() {
    let (first, first_seen) = recording_tool("foo_bar");
    let (second, second_seen) = recording_tool("foo-bar");

    let outcome = call_once("mcp__686f__FooBar", vec![first, second], |_| {}).await;

    assert!(first_seen.lock().unwrap().is_empty());
    assert!(second_seen.lock().unwrap().is_empty());
    assert!(outcome.result.is_error);
    assert_eq!(text_of(&outcome.result), "Tool mcp__686f__FooBar not found");
}

#[tokio::test]
async fn uses_the_canonical_name_of_a_tool_the_host_resolver_activates_for_a_namespaced_call() {
    let (lazy, seen) = recording_tool("lazy_weather");
    let resolver_tool = lazy.clone();
    let resolved_names: Arc<std::sync::Mutex<Vec<String>>> = Arc::new(std::sync::Mutex::new(Vec::new()));
    let resolver_sink = Arc::clone(&resolved_names);
    let resolver: ResolveUnknownToolCall = Arc::new(move |name: String, _context: AgentContext| {
        resolver_sink
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(name.clone());
        let tool = resolver_tool.clone();
        Box::pin(async move {
            if name == "mcp__686f__lazy_weather" {
                Some(tool)
            } else {
                None
            }
        })
    });

    let outcome = call_once("mcp__686f__lazy_weather", Vec::new(), move |config| {
        config.resolve_unknown_tool_call = Some(resolver);
    })
    .await;

    assert_eq!(*resolved_names.lock().unwrap(), vec!["mcp__686f__lazy_weather".to_owned()]);
    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(outcome.result.tool_name, "lazy_weather");
    assert_eq!(outcome.start_tool_names, vec!["lazy_weather".to_owned()]);
    assert_eq!(outcome.end_tool_names, vec!["lazy_weather".to_owned()]);
    assert!(text_of(&outcome.result).contains("[auto-corrected]"));
}

#[tokio::test]
async fn leaves_exact_name_calls_untouched() {
    let (tool, seen) = recording_tool("lazy_weather");

    let outcome = call_once("lazy_weather", vec![tool], |_| {}).await;

    assert_eq!(seen.lock().unwrap().len(), 1);
    assert_eq!(outcome.result.tool_name, "lazy_weather");
    assert_eq!(text_of(&outcome.result), "lazy_weather:Seoul");
}

#[tokio::test]
async fn alias_resolution_rejects_incomplete_calls() {
    let mut incomplete = tool_call("call-1", "mcp__686f__lazy_weather", json!({ "city": "Seoul" }));
    if let ContentBlock::ToolCall(call) = &mut incomplete {
        call.incomplete = Some(true);
    }
    let (tool, seen) = recording_tool("lazy_weather");
    let context = AgentContext { system_prompt: String::new(), messages: Vec::new(), tools: Some(vec![tool]) };
    let config = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    let call = match &incomplete {
        ContentBlock::ToolCall(call) => call.clone(),
        _ => unreachable!(),
    };
    assert!(maho_agent::tool_name_alias::resolve_call_tool(&context, &call, &config).await.is_none());
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn strips_only_a_single_segment_gateway_namespace() {
    let names = vec!["lazy_weather".to_owned()];
    assert_eq!(
        maho_agent::tool_name_alias::resolve_tool_name_alias("mcp__686f__lazy_weather", names.clone()),
        Some("lazy_weather".to_owned())
    );
    assert_eq!(
        maho_agent::tool_name_alias::resolve_tool_name_alias("mcp__68_6f__lazy_weather", names.clone()),
        None
    );
    assert_eq!(maho_agent::tool_name_alias::resolve_tool_name_alias("mcp__lazy_weather", names), None);
}

#[test]
fn builds_the_correction_notice_with_the_requested_and_resolved_names() {
    assert_eq!(
        maho_agent::tool_name_alias::tool_name_correction_notice("LazyWeather", "lazy_weather"),
        "[auto-corrected] no tool is named \"LazyWeather\"; ran \"lazy_weather\". Call tools by their exact listed name."
    );
}

#[test]
fn with_tool_name_correction_prepends_a_model_only_block() {
    let result = AgentToolResult::text("lazy_weather:Seoul");
    let corrected = maho_agent::tool_name_alias::with_tool_name_correction(result, "LazyWeather", "lazy_weather");
    assert_eq!(corrected.content.len(), 2);
    assert_eq!(corrected.content[0], expected_correction("LazyWeather", "lazy_weather"));
    assert_eq!(corrected.content[1], text_block("lazy_weather:Seoul"));
}

#[tokio::test]
async fn a_hook_that_returns_replacement_args_runs_the_tool_with_them() {
    let (tool, seen) = recording_tool("lazy_weather");
    let outcome = call_once("lazy_weather", vec![tool], move |config| {
        config.before_tool_call = Some(Arc::new(move |_context: BeforeToolCallContext, _signal: Option<AbortSignal>| {
            Box::pin(async { Some(BeforeToolCallResult { args: Some(json!({ "city": "Busan" })), ..Default::default() }) })
        }));
    }).await;
    assert_eq!(*seen.lock().unwrap(), vec![json!({ "city": "Busan" })]);
    assert!(!outcome.result.is_error);
}

#[tokio::test]
async fn a_hook_that_returns_no_args_leaves_the_validated_arguments_untouched() {
    let (tool, seen) = recording_tool("lazy_weather");
    let outcome = call_once("lazy_weather", vec![tool], move |config| {
        config.before_tool_call = Some(Arc::new(move |_context: BeforeToolCallContext, _signal: Option<AbortSignal>| {
            Box::pin(async { Some(BeforeToolCallResult::default()) })
        }));
    }).await;
    assert_eq!(*seen.lock().unwrap(), vec![json!({ "city": "Seoul" })]);
    assert!(!outcome.result.is_error);
}

#[tokio::test]
async fn a_blocking_hook_prevents_execution_and_reports_the_reason() {
    let (tool, seen) = recording_tool("lazy_weather");
    let outcome = call_once("lazy_weather", vec![tool], move |config| {
        config.before_tool_call = Some(Arc::new(move |_context: BeforeToolCallContext, _signal: Option<AbortSignal>| {
            Box::pin(async { Some(BeforeToolCallResult { block: Some(true), reason: Some("blocked by hook".into()), ..Default::default() }) })
        }));
    }).await;
    assert!(seen.lock().unwrap().is_empty());
    assert!(outcome.result.is_error);
    assert!(text_of(&outcome.result).contains("blocked by hook"));
}
