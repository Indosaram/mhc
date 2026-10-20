use maho_ai::{types::AssistantMessageEvent, utils::event_stream::AssistantMessageEventStream};
pub const DEFAULT_ESTABLISHMENT_TIMEOUT_MS: u64 = 30_000;
pub const SIDE_QUERY_INSTRUCTION: &str = "The user is asking a side question about the conversation so far, outside the main task. Answer it directly and concisely from the context above. Do not continue any task, do not modify anything, and do not treat this as new work.";
pub fn get_side_query_prompt_context_window(model: &maho_ai::types::Model) -> f64 {
    maho_ext_compaction::extension_wiring::get_prompt_context_window(
        model.context_window as f64,
        Some(model.max_tokens as f64),
    )
}
pub fn build_side_query_context(
    system_prompt: &str,
    history: Vec<maho_ai::types::Message>,
    question: &str,
    model: &maho_ai::types::Model,
) -> Result<maho_ai::types::Context, String> {
    let system_prompt = format!("{system_prompt}\n\n{SIDE_QUERY_INSTRUCTION}");
    let window = get_side_query_prompt_context_window(model);
    let system_tokens = maho_core::compaction::compaction::estimate_tokens(
        &serde_json::json!({"role":"user","content":system_prompt,"timestamp":0}),
    );
    let mut messages = history;
    messages.push(serde_json::from_value(serde_json::json!({"role":"user","content":question,"timestamp":maho_ai::utils::diagnostics::now_ms()})).map_err(|error|error.to_string())?);
    if !window.is_finite() || window <= 0.0 {
        return Ok(maho_ai::types::Context {
            system_prompt: Some(system_prompt),
            messages,
            tools: Some(vec![]),
        });
    }
    let estimate = |messages: &[maho_ai::types::Message]| {
        messages
            .iter()
            .map(|message| {
                maho_core::compaction::compaction::estimate_tokens(
                    &serde_json::to_value(message).expect("message"),
                )
            })
            .sum::<u64>()
    };
    let budget = (window - system_tokens as f64).max(0.0) as u64;
    if estimate(&messages[messages.len() - 1..]) > budget {
        return Err("/btw question does not fit this model's context window; shorten it or run /compact first.".into());
    }
    if estimate(&messages) > budget {
        let reduced = maho_ext_compaction::context_reduction::reduce_context_messages(
            &messages,
            &maho_ext_compaction::context_reduction::ReduceContextOptions::builtin(),
        );
        let repaired =
            maho_ai::utils::tool_pair_repair::repair_orphaned_tool_results(&reduced.messages);
        let raw = repaired
            .iter()
            .map(|message| serde_json::to_value(message).expect("message"))
            .collect::<Vec<_>>();
        let pruned =
            maho_ext_compaction::overflow_retry::prune_old_messages_to_budget(&raw, budget);
        messages = maho_ai::utils::tool_pair_repair::repair_orphaned_tool_results(
            &pruned
                .into_iter()
                .map(|message| serde_json::from_value(message).map_err(|error| error.to_string()))
                .collect::<Result<Vec<_>, _>>()?,
        );
        if estimate(&messages) > budget {
            return Err("/btw context is too large for this model; run /compact first.".into());
        }
    }
    Ok(maho_ai::types::Context {
        system_prompt: Some(system_prompt),
        messages,
        tools: Some(vec![]),
    })
}
pub async fn collect_reply(
    stream: &AssistantMessageEventStream,
    timeout_ms: u64,
    mut on_text_delta: impl FnMut(&str),
) -> Result<String, String> {
    let outcome = async {
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let mut established = false;
    let mut reply = String::new();
    loop {
        let event = if established {
            stream.next().await
        } else {
            match tokio::time::timeout_at(deadline, stream.next()).await {
                Ok(event) => event,
                Err(_) => {
                    return Err(format!(
                        "/btw provider did not produce an event within {}s",
                        (timeout_ms as f64 / 1000.0).round()
                    ));
                }
            }
        }
        .map_err(|error| error.message)?;
        let Some(event) = event else { break };
        if !matches!(event, AssistantMessageEvent::Start { .. }) {
            established = true;
        }
        match event {
            AssistantMessageEvent::TextDelta { delta, .. } => {
                reply.push_str(&delta);
                on_text_delta(&delta);
            }
            AssistantMessageEvent::Done { .. } => break,
            AssistantMessageEvent::Error { error, .. } => {
                return Err(error
                    .error_message
                    .filter(|message| !message.is_empty())
                    .unwrap_or_else(|| "Side query failed".into()));
            }
            _ => {}
        }
    }
    Ok(reply)
    }.await;
    stream.cancel().await.map_err(|error| error.message)?;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn terminal_events_wait_for_cleanup_before_returning() {
        for failed in [false, true] {
            let stream = AssistantMessageEventStream::assistant();
            let message = serde_json::from_value(serde_json::json!({
                "role":"assistant", "content":[], "api":"faux", "provider":"faux", "model":"fixture",
                "usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,
                    "cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},
                "stopReason":if failed { "error" } else { "stop" },
                "errorMessage":if failed { Some("provider failed") } else { None },
                "errorCause":null, "timestamp":0
            })).expect("message");
            let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = tokio::sync::oneshot::channel();
            stream.set_cancellation_handler(Box::new(move || Box::pin(async move {
                entered_tx.send(()).expect("cleanup subscriber");
                release_rx.await.expect("cleanup release");
                Ok(())
            })));
            if failed {
                stream.push(AssistantMessageEvent::Error {
                    reason: maho_ai::types::ErrorReason::Error,
                    error: message,
                });
            } else {
                stream.push(AssistantMessageEvent::Done {
                    reason: maho_ai::types::DoneReason::Stop,
                    message,
                });
            }
            let task = tokio::spawn(async move { collect_reply(&stream, 30_000, |_| {}).await });
            let bound = std::time::Duration::from_secs(5);
            tokio::time::timeout(bound, entered_rx).await.expect("cleanup deadline").expect("cleanup");
            assert!(!task.is_finished());
            release_tx.send(()).expect("cleanup subscriber");
            let outcome = tokio::time::timeout(bound, task).await.expect("reply deadline").expect("task");
            assert_eq!(outcome, if failed { Err("provider failed".into()) } else { Ok(String::new()) });
        }
    }

    #[tokio::test]
    async fn reply_waits_for_terminal_cleanup_and_owned_relay() {
        let stream = AssistantMessageEventStream::assistant();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let (relay_tx, relay_rx) = tokio::sync::oneshot::channel();
        stream.set_cancellation_handler(Box::new(move || Box::pin(async move {
            entered_tx.send(()).expect("cleanup subscriber");
            release_rx.await.expect("cleanup release");
            Ok(())
        })));
        stream.set_cancellation_task(tokio::spawn(async move {
            relay_rx.await.expect("relay release");
        }));
        stream.end(None);
        let task = tokio::spawn(async move { collect_reply(&stream, 30_000, |_| {}).await });
        let bound = std::time::Duration::from_secs(5);
        tokio::time::timeout(bound, entered_rx).await.expect("cleanup deadline").expect("cleanup");
        assert!(!task.is_finished());
        release_tx.send(()).expect("cleanup subscriber");
        assert!(!task.is_finished());
        relay_tx.send(()).expect("relay subscriber");
        assert_eq!(tokio::time::timeout(bound, task).await.expect("reply deadline").expect("task").expect("reply"), "");
    }

    #[tokio::test]
    async fn failed_stream_waits_for_cleanup_before_returning_error() {
        let stream = AssistantMessageEventStream::assistant();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        stream.set_cancellation_handler(Box::new(move || Box::pin(async move {
            entered_tx.send(()).expect("cleanup subscriber");
            release_rx.await.expect("cleanup release");
            Ok(())
        })));
        stream.fail(maho_ai::utils::event_stream::StreamError::new("failed"));
        let task = tokio::spawn(async move { collect_reply(&stream, 30_000, |_| {}).await });
        let bound = std::time::Duration::from_secs(5);
        tokio::time::timeout(bound, entered_rx).await.expect("cleanup deadline").expect("cleanup");
        assert!(!task.is_finished());
        release_tx.send(()).expect("cleanup subscriber");
        assert_eq!(tokio::time::timeout(bound, task).await.expect("reply deadline").expect("task").expect_err("failure"), "failed");
    }
}
