use maho_ai::types::{Model, ModelThinkingLevel};
use serde_json::{Value, json};

pub fn summary_max_tokens(model: &Model, context_window: u64) -> u64 {
    let headroom = if model.max_tokens > 0 {
        model.max_tokens.min(32_768)
    } else {
        32_768
    };
    if context_window > 0 {
        headroom.min(context_window / 2)
    } else {
        headroom
    }
}

pub fn summarization_reasoning_options(model: &Model) -> Value {
    if !model.reasoning {
        return json!({});
    }
    if model.api == "anthropic-messages" {
        return json!({"thinkingEnabled":false});
    }
    let effort = [
        (ModelThinkingLevel::Low, "low"),
        (ModelThinkingLevel::Medium, "medium"),
        (ModelThinkingLevel::High, "high"),
    ]
    .into_iter()
    .find(|(level, _)| {
        !matches!(
            model
                .thinking_level_map
                .as_ref()
                .and_then(|map| map.get(level)),
            Some(None)
        )
    });
    let Some((_, effort)) = effort else {
        return json!({});
    };
    match model.api.as_str() {
        "openai-responses" | "openai-codex-responses" | "azure-openai-responses" => {
            json!({"reasoningEffort":effort,"reasoningSummary":null})
        }
        "openai-completions" => json!({"reasoningEffort":effort}),
        _ => json!({}),
    }
}

pub fn has_summarization_reasoning_override(model: &Model) -> bool {
    summarization_reasoning_options(model)
        .as_object()
        .is_some_and(|options| !options.is_empty())
}

pub fn get_summary_text(message: &Value) -> String {
    if let Some(text) = message.get("content").and_then(Value::as_str) {
        return text.trim().to_owned();
    }
    message
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|block| block.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|block| block.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_owned()
}

pub fn is_assistant_message(message: &Value) -> bool {
    message.get("role").and_then(Value::as_str) == Some("assistant")
        && message.get("stopReason").is_some()
}

#[derive(Debug)]
pub enum SummaryStreamError {
    Provider(maho_ai::utils::event_stream::StreamError),
    IdleTimeout,
    DurationBudget,
}

pub async fn consume_summary_stream(
    stream: &maho_ai::types::AssistantMessageEventStream,
    controller: &maho_ai::utils::abort::AbortController,
    caller_signal: Option<&maho_ai::utils::abort::AbortSignal>,
    idle_timeout: std::time::Duration,
    max_duration: std::time::Duration,
    on_progress: &(dyn Fn(&str) + Sync),
) -> Result<Option<maho_ai::types::AssistantMessage>, SummaryStreamError> {
    let idle = tokio::time::sleep(idle_timeout);
    let duration = tokio::time::sleep(max_duration);
    tokio::pin!(idle, duration);
    let cancelled = async {
        match caller_signal {
            Some(signal) => signal.cancelled().await,
            None => std::future::pending::<()>().await,
        }
    };
    tokio::pin!(cancelled);
    loop {
        tokio::select! {
            biased;
            () = &mut cancelled => { controller.abort(None); stream.cancel().await.map_err(SummaryStreamError::Provider)?; return Ok(None); }
            () = &mut idle => { controller.abort(None); stream.cancel().await.map_err(SummaryStreamError::Provider)?; return Err(SummaryStreamError::IdleTimeout); }
            () = &mut duration => { controller.abort(None); stream.cancel().await.map_err(SummaryStreamError::Provider)?; return Err(SummaryStreamError::DurationBudget); }
            event = stream.next() => match event {
                Ok(Some(event)) => {
                    idle.as_mut().reset(tokio::time::Instant::now() + idle_timeout);
                    if let maho_ai::types::AssistantMessageEvent::TextDelta { delta, .. } = event
                        && !delta.is_empty() { on_progress(&delta); }
                }
                Ok(None) => break,
                Err(error) => {
                    controller.abort(None);
                    stream.cancel().await.map_err(SummaryStreamError::Provider)?;
                    if caller_signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) {
                        return Ok(None);
                    }
                    return Err(SummaryStreamError::Provider(error));
                }
            }
        }
    }
    tokio::select! {
        biased;
        () = &mut cancelled => { controller.abort(None); stream.cancel().await.map_err(SummaryStreamError::Provider)?; Ok(None) }
        () = &mut idle => { controller.abort(None); stream.cancel().await.map_err(SummaryStreamError::Provider)?; Err(SummaryStreamError::IdleTimeout) }
        () = &mut duration => { controller.abort(None); stream.cancel().await.map_err(SummaryStreamError::Provider)?; Err(SummaryStreamError::DurationBudget) }
        result = stream.result() => {
            // Done/Error settles the message, not the provider relay's resources.
            stream.cancel().await.map_err(SummaryStreamError::Provider)?;
            if caller_signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) {
                Ok(None)
            } else {
                result.map(Some).map_err(SummaryStreamError::Provider)
            }
        },
    }
}

pub type SummaryStreamRunner = std::sync::Arc<
    dyn Fn(
            &Model,
            &maho_ai::types::Context,
            maho_ai::types::StreamOptions,
        ) -> maho_ai::types::AssistantMessageEventStream
        + Send
        + Sync,
>;

pub struct SummaryRequestOptions<'a> {
    pub snapshot: &'a crate::speculative::SpeculativeCompactionSnapshot,
    pub messages: &'a [Value],
    pub prompt: &'a crate::prompts::CompactionPrompt,
    pub api_key: Option<String>,
    pub headers: Option<maho_ai::types::ProviderHeaders>,
    pub extra_body: Option<serde_json::Map<String, Value>>,
    pub signal: Option<&'a maho_ai::utils::abort::AbortSignal>,
    pub max_duration: std::time::Duration,
    pub omit_reasoning_options: bool,
    pub forbid_tool_calls: bool,
    pub stream_runner: Option<&'a SummaryStreamRunner>,
}

pub async fn generate_summary_message(
    options: SummaryRequestOptions<'_>,
    on_progress: &(dyn Fn(&str) + Sync),
) -> Result<Option<maho_ai::types::AssistantMessage>, SummaryStreamError> {
    let mut messages = options.messages.to_vec();
    messages.push(json!({"role":"user","content":[{"type":"text","text":options.prompt.user}],"timestamp":chrono::Utc::now().timestamp_millis()}));
    let mut typed = maho_core::messages::convert_to_llm(&messages)
        .into_iter()
        .map(serde_json::from_value)
        .collect::<Result<Vec<maho_ai::types::Message>, _>>()
        .map_err(|error| {
            SummaryStreamError::Provider(maho_ai::utils::event_stream::StreamError::new(
                error.to_string(),
            ))
        })?;
    // Signed thinking is bound to the conversation that produced it. The summary
    // request has its own system prompt and tools, and runs with thinking off,
    // which on adaptive-only models also drops the `drop_block` binding control;
    // replaying the signatures would then fail the whole request with a 400.
    for message in &mut typed {
        if let maho_ai::types::Message::Assistant(assistant) = message {
            assistant
                .content
                .retain(|block| !matches!(block, maho_ai::types::ContentBlock::Thinking(_)));
        }
    }
    let messages = crate::repair_tool_pairs::repair_orphaned_tool_results(
        &crate::summarization_turn_order::normalize_summarization_turn_order(&typed),
        chrono::Utc::now().timestamp_millis(),
    );
    let context = maho_ai::types::Context {
        active_tool_names: None,
        system_prompt: Some(
            options
                .snapshot
                .system_prompt
                .as_deref()
                .unwrap_or(options.prompt.system)
                .into(),
        ),
        messages,
        tools: (!options.snapshot.tools.is_empty()).then(|| options.snapshot.tools.clone()),
    };
    let controller = maho_ai::utils::abort::AbortController::new();
    let mut stream_options = maho_ai::types::StreamOptions {
        max_tokens: Some(summary_max_tokens(
            &options.snapshot.model,
            options.snapshot.context_window,
        )),
        extra_body: options.extra_body,
        request: maho_ai::types::ProviderRequestOptions {
            api_key: options.api_key,
            headers: options.headers,
            signal: Some(controller.signal()),
            on_payload: Some(std::sync::Arc::new(|payload, model, _| {
                if model.api == "anthropic-messages" {
                    payload.as_object().map(|payload| {
                        Value::Object(
                            maho_ai::api::anthropic_tool_pairs::sanitize_anthropic_tool_pairs(
                                payload,
                            ),
                        )
                    })
                } else {
                    Some(payload.clone())
                }
            })),
            ..Default::default()
        },
        ..Default::default()
    };
    if !options.omit_reasoning_options
        && let Some(reasoning) =
            summarization_reasoning_options(&options.snapshot.model).as_object()
    {
        stream_options.extra.extend(reasoning.clone());
    }
    if options.forbid_tool_calls {
        stream_options
            .extra
            .insert("toolChoice".into(), json!("none"));
    }
    let stream = match options.stream_runner {
        Some(runner) => runner(&options.snapshot.model, &context, stream_options),
        None => maho_ai::compat::stream(&options.snapshot.model, &context, Some(stream_options)),
    };
    consume_summary_stream(
        &stream,
        &controller,
        options.signal,
        std::time::Duration::from_millis(300_000),
        options.max_duration,
        on_progress,
    )
    .await
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use maho_ai::{
        types::{AssistantMessage, AssistantMessageEvent, DoneReason},
        utils::{abort::AbortController, event_stream::{StreamError, create_assistant_message_event_stream}},
    };
    use std::sync::{Arc, atomic::{AtomicBool, Ordering}};
    use std::time::Duration;

    #[tokio::test]
    async fn every_summary_exit_awaits_cleanup_and_relay_release() {
        for exit in ["done", "error", "abort"] {
            // Given a settled or cancelled stream with cleanup held at an exact barrier.
            let stream = create_assistant_message_event_stream();
            let (started, cleanup_started) = tokio::sync::oneshot::channel();
            let (release, cleanup_release) = tokio::sync::oneshot::channel();
            let (stop_relay, relay_stopped) = tokio::sync::oneshot::channel();
            stream.set_cancellation_handler(Box::new(move || Box::pin(async move {
                started.send(()).expect("cleanup observed");
                cleanup_release.await.expect("cleanup release");
                stop_relay.send(()).expect("relay still owned");
                Ok(())
            })));
            let released = Arc::new(AtomicBool::new(false));
            let observed = Arc::clone(&released);
            stream.set_cancellation_task(tokio::spawn(async move {
                relay_stopped.await.expect("relay stop");
                observed.store(true, Ordering::SeqCst);
            }));
            let caller = AbortController::new();
            match exit {
                "done" => {
                    let response: AssistantMessage = serde_json::from_value(json!({
                        "content":[],"api":"faux","provider":"faux","model":"faux",
                        "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
                            "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
                        "stopReason":"stop","timestamp":0
                    })).expect("assistant fixture");
                    stream.push(AssistantMessageEvent::Done { reason: DoneReason::Stop, message: response });
                }
                "error" => stream.fail(StreamError::new("provider failed")),
                "abort" => caller.abort(None),
                _ => unreachable!("test case"),
            }

            // When the consumer reaches its abandonment boundary.
            let consumer = tokio::spawn(async move {
                consume_summary_stream(&stream, &AbortController::new(), Some(&caller.signal()),
                    Duration::from_secs(30), Duration::from_secs(30), &|_| {}).await
            });
            tokio::time::timeout(Duration::from_secs(5), cleanup_started).await
                .expect("cleanup starts").expect("cleanup notification");

            // Then no result escapes before both cleanup and the relay have finished.
            assert!(!consumer.is_finished());
            release.send(()).expect("release cleanup");
            let result = tokio::time::timeout(Duration::from_secs(5), consumer).await
                .expect("consumer settles").expect("consumer task");
            assert!(released.load(Ordering::SeqCst));
            match exit {
                "done" => assert!(result.expect("summary").is_some()),
                "error" => assert!(matches!(result, Err(SummaryStreamError::Provider(_)))),
                "abort" => assert!(result.expect("stand down").is_none()),
                _ => unreachable!("test case"),
            }
        }
    }
}
