//! Port of senpi `packages/agent/test/{step-commit-error,z1-commit-step,ck1p-agent-commit-step,tool-host-seam,proxy-tool-arguments-reset,agent-loop-transport-keepalive}.test.ts`.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use maho_agent::execution::tool_host::{
    ExecutionPipelineWitness, ExecutionScope, ToolHost, ToolHostCall, ToolHostOptions,
    ToolHostResult,
};
use maho_agent::proxy::ProxyStreamOptions;
use maho_agent::proxy_event_reducer::process_proxy_event;
use maho_agent::step_commit::{
    StepCommit, StepCommitError, StepCommitFailure, is_step_commit_error,
};
use maho_agent::types::AgentLoopConfig;
use maho_agent::{
    Agent, AgentContinuationOptions, AgentEvent, AgentMessage, AgentOptions, AgentPromptInput,
    AgentTool, AgentToolResult, PartialAgentState, ToolExecutionMode, identity_convert_to_llm,
    run_agent_loop, stream_proxy,
};
use maho_ai::types::{
    AssistantMessageEvent, ContentBlock, Message, StopReason, ToolResultMessage, UserContent,
    UserMessage,
};
use maho_ai::utils::event_stream::{AssistantMessageEventStream, StreamError};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use crate::support::{
    assistant, message_stream, scripted_stream_fn, test_model, text_block, tool_call, user_message,
};

fn event_kind(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::AgentStart => "agent_start",
        AgentEvent::AgentEnd { .. } => "agent_end",
        AgentEvent::TurnStart => "turn_start",
        AgentEvent::TurnEnd { .. } => "turn_end",
        AgentEvent::MessageStart { .. } => "message_start",
        AgentEvent::MessageUpdate { .. } => "message_update",
        AgentEvent::MessageEnd { .. } => "message_end",
        AgentEvent::ToolExecutionStart { .. } => "tool_execution_start",
        AgentEvent::ToolExecutionUpdate { .. } => "tool_execution_update",
        AgentEvent::ToolExecutionEnd { .. } => "tool_execution_end",
    }
}

#[test]
fn step_commit_error_brand_matches_step_commit_error_and_rejects_other_errors() {
    let inner = std::io::Error::other("inner");
    let err = StepCommitError::with_cause("s", StepCommitFailure::Generation, Arc::new(inner));
    assert_eq!(err.code, StepCommitFailure::Generation);
    assert_eq!(err.step_id, "s");
    assert!(is_step_commit_error(&err));
    let other = std::io::Error::other("step commit failed: fenced");
    assert!(!is_step_commit_error(&other));
}

#[tokio::test]
async fn tool_host_failure_has_distinct_error_classification() {
    struct FailingHost;
    impl ToolHost for FailingHost {
        fn execute(
            &self,
            _: ToolHostCall,
            _: ToolHostOptions,
        ) -> maho_ai::types::BoxFuture<'_, Result<ToolHostResult, String>> {
            Box::pin(async { Err("host-fault-17".to_owned()) })
        }
    }
    let mut cfg = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    cfg.tool_host = Some(Arc::new(FailingHost));
    let sink: maho_agent::AgentEventSink = Arc::new(|_| Box::pin(async {}));
    let err = run_agent_loop(
        vec![user_message("go")],
        maho_agent::AgentContext {
            system_prompt: String::new(),
            system_prompt_stable_prefix: None,
            messages: Vec::new(),
            tools: None,
            declared_tools: None,
        },
        cfg,
        sink,
        None,
        Some(scripted_stream_fn(vec![assistant(
            vec![tool_call("call-1", "echo", json!({}))],
            StopReason::ToolUse,
        )])),
    )
    .await
    .expect_err("host failure propagates");
    assert!(matches!(
        err,
        maho_agent::AgentLoopError::ToolHost(message) if message == "host-fault-17"
    ));
}

#[tokio::test]
async fn commit_step_unset_leaves_loop_unchanged_and_set_commits_before_tool_dispatch_and_turn_end()
{
    let trace: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let executed: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let exec_clone = executed.clone();
    let echo = AgentTool {
        label: "echo".to_owned(),
        prepare_arguments: None,
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "echo".to_owned(),
            description: "echo".to_owned(),
            parameters: json!({ "type": "object" }),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(move |id, _, _, _| {
            let exec_clone = exec_clone.clone();
            Box::pin(async move {
                exec_clone.lock().expect("executed").push(id);
                AgentToolResult::text("ok")
            })
        }),
    };

    let committed: Arc<std::sync::Mutex<Vec<StopReason>>> = Arc::default();
    let c_clone = committed.clone();
    let t_commit = trace.clone();
    let mut cfg = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    cfg.commit_step = Some(Arc::new(move |step: StepCommit| {
        let c_clone = c_clone.clone();
        let t_commit = t_commit.clone();
        Box::pin(async move {
            c_clone
                .lock()
                .expect("committed")
                .push(step.message.stop_reason);
            t_commit.lock().expect("trace").push(format!(
                "commit:{:?}:{}",
                step.message.stop_reason,
                step.provider_tool_results.len()
            ));
            Ok(())
        })
    }));
    let t_sink = trace.clone();
    let sink: maho_agent::AgentEventSink = Arc::new(move |event| {
        let t_sink = t_sink.clone();
        Box::pin(async move {
            let label = match &event {
                AgentEvent::MessageEnd { message } => format!("message_end:{}", message.role()),
                AgentEvent::ToolExecutionStart { tool_call_id, .. } => {
                    format!("tool_execution_start:{tool_call_id}")
                }
                AgentEvent::TurnEnd { .. } => "turn_end".to_owned(),
                other => event_kind(other).to_owned(),
            };
            t_sink.lock().expect("trace").push(label);
        })
    });

    let messages = run_agent_loop(
        vec![user_message("go")],
        maho_agent::AgentContext {
            system_prompt: String::new(),
            system_prompt_stable_prefix: None,
            messages: Vec::new(),
            tools: Some(vec![echo]),
            declared_tools: None,
        },
        cfg,
        sink,
        None,
        Some(scripted_stream_fn(vec![
            assistant(
                vec![tool_call("call-1", "echo", json!({}))],
                StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], StopReason::Stop),
        ])),
    )
    .await
    .expect("commit_step loop succeeds");

    assert_eq!(
        *executed.lock().expect("executed"),
        vec!["call-1".to_owned()]
    );
    assert_eq!(
        *committed.lock().expect("committed"),
        vec![StopReason::ToolUse, StopReason::Stop]
    );
    let trace = trace.lock().expect("trace").clone();
    let commit_idx = trace
        .iter()
        .position(|s| s == "commit:ToolUse:0")
        .expect("commit recorded");
    let tool_idx = trace
        .iter()
        .position(|s| s == "tool_execution_start:call-1")
        .expect("tool started");
    assert!(commit_idx < tool_idx);
    assert!(messages.iter().any(|m| m.role() == "toolResult"));
}

#[tokio::test]
async fn rejected_commit_step_exits_with_step_commit_error_and_never_dispatches_tools() {
    let ran = Arc::new(AtomicUsize::new(0));
    let ran_clone = ran.clone();
    let echo = AgentTool {
        label: "echo".to_owned(),
        prepare_arguments: None,
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "echo".to_owned(),
            description: "echo".to_owned(),
            parameters: json!({ "type": "object" }),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(move |_, _, _, _| {
            let ran_clone = ran_clone.clone();
            Box::pin(async move {
                ran_clone.fetch_add(1, Ordering::SeqCst);
                AgentToolResult::text("ok")
            })
        }),
    };

    let mut cfg = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    cfg.commit_step = Some(Arc::new(|_| {
        Box::pin(async { Err(StepCommitError::new("step-1", StepCommitFailure::Fenced)) })
    }));
    let trace: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let t_sink = trace.clone();
    let sink: maho_agent::AgentEventSink = Arc::new(move |event| {
        let t_sink = t_sink.clone();
        Box::pin(async move {
            t_sink
                .lock()
                .expect("trace")
                .push(event_kind(&event).to_owned());
        })
    });

    let err = run_agent_loop(
        vec![user_message("go")],
        maho_agent::AgentContext {
            system_prompt: String::new(),
            system_prompt_stable_prefix: None,
            messages: Vec::new(),
            tools: Some(vec![echo]),
            declared_tools: None,
        },
        cfg,
        sink,
        None,
        Some(scripted_stream_fn(vec![assistant(
            vec![tool_call("call-1", "echo", json!({}))],
            StopReason::ToolUse,
        )])),
    )
    .await
    .expect_err("rejected commit fails loop");

    assert_eq!(ran.load(Ordering::SeqCst), 0);
    assert!(matches!(
        err,
        maho_agent::AgentLoopError::StepCommit(StepCommitError {
            code: StepCommitFailure::Fenced,
            ..
        })
    ));
    assert!(
        !trace
            .lock()
            .expect("trace")
            .contains(&"turn_end".to_owned())
    );
}

#[tokio::test]
async fn agent_forwards_commit_step_to_prompt_queued_followup_and_continue() {
    let commits = Arc::new(std::sync::Mutex::new(Vec::<StopReason>::new()));
    let c_clone = commits.clone();
    let turn = Arc::new(AtomicUsize::new(0));
    let t_clone = turn.clone();

    let agent = Agent::new(AgentOptions {
        initial_state: Some(PartialAgentState {
            model: Some(test_model()),
            ..Default::default()
        }),
        stream_fn: Some(Arc::new(move |_, _, _| {
            let idx = t_clone.fetch_add(1, Ordering::SeqCst);
            let label = format!("reply-{idx}");
            message_stream(assistant(vec![text_block(&label)], StopReason::Stop))
        })),
        commit_step: Some(Arc::new(move |step| {
            let c_clone = c_clone.clone();
            Box::pin(async move {
                c_clone
                    .lock()
                    .expect("commits")
                    .push(step.message.stop_reason);
                Ok(())
            })
        })),
        ..Default::default()
    });

    let queued = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let q_clone = queued.clone();
    let agent_sub = agent.clone();
    agent.subscribe(Arc::new(move |event, _| {
        let q_clone = q_clone.clone();
        let agent_sub = agent_sub.clone();
        Box::pin(async move {
            if matches!(event, AgentEvent::AgentEnd { .. }) && !q_clone.swap(true, Ordering::SeqCst)
            {
                agent_sub.follow_up(AgentMessage::Llm(Message::User(UserMessage {
                    content: UserContent::Text("next".to_owned()),
                    timestamp: 2,
                })));
            }
        })
    }));

    agent.prompt(AgentPromptInput::text("go")).await;
    agent.wait_for_idle().await;
    assert_eq!(
        *commits.lock().expect("commits"),
        vec![StopReason::Stop, StopReason::Stop]
    );

    agent.set_messages(vec![user_message("resume")]);
    agent
        .continue_run(AgentContinuationOptions::default())
        .await
        .expect("continue");
    assert_eq!(
        *commits.lock().expect("commits"),
        vec![StopReason::Stop, StopReason::Stop, StopReason::Stop]
    );
}

#[tokio::test]
async fn unprepared_tool_host_seam_bypasses_local_prep_and_default_pipeline_records_witness() {
    let trace: Arc<std::sync::Mutex<Vec<String>>> = Arc::default();
    let t_prep = trace.clone();
    let t_body = trace.clone();
    let mut read_tool = AgentTool {
        label: "read".to_owned(),
        prepare_arguments: None,
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "read".to_owned(),
            description: "fixture".to_owned(),
            parameters: json!({ "type": "object", "properties": { "path": { "type": "string" } } }),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(move |_, _, _, _| {
            let t_body = t_body.clone();
            Box::pin(async move {
                t_body.lock().expect("trace").push("body".to_owned());
                AgentToolResult::text("local")
            })
        }),
    };
    read_tool.prepare_arguments = Some(Arc::new(move |_| {
        t_prep.lock().expect("trace").push("prepare".to_owned());
        Ok(json!({ "path": "prepared" }))
    }));

    struct CustomHost {
        received: Arc<std::sync::Mutex<Vec<ToolHostCall>>>,
    }
    impl ToolHost for CustomHost {
        fn execute(
            &self,
            call: ToolHostCall,
            options: ToolHostOptions,
        ) -> maho_ai::types::BoxFuture<'_, Result<ToolHostResult, String>> {
            self.received.lock().expect("received").push(call.clone());
            Box::pin(async move {
                let message = ToolResultMessage {
                    tool_call_id: "call-1".to_owned(),
                    tool_name: "read".to_owned(),
                    content: vec![text_block("remote")],
                    details: None,
                    is_error: false,
                    timestamp: 2,
                    usage: None,
                    nested_calls: None,
                    added_tool_names: None,
                };
                (options.emit)(AgentEvent::MessageStart {
                    message: AgentMessage::Llm(Message::ToolResult(message.clone())),
                    streaming: None,
                })
                .await;
                (options.emit)(AgentEvent::MessageEnd {
                    message: AgentMessage::Llm(Message::ToolResult(message.clone())),
                })
                .await;
                Ok(ToolHostResult {
                    messages: vec![message],
                    terminate: false,
                })
            })
        }
    }

    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let agent = Agent::new(AgentOptions {
        initial_state: Some(PartialAgentState {
            model: Some(test_model()),
            tools: Some(vec![read_tool.clone()]),
            ..Default::default()
        }),
        stream_fn: Some(scripted_stream_fn(vec![
            assistant(
                vec![tool_call("call-1", "read", json!({ "raw": "a" }))],
                StopReason::ToolUse,
            ),
            assistant(vec![text_block("done")], StopReason::Stop),
        ])),
        tool_host: Some(Arc::new(CustomHost {
            received: received.clone(),
        })),
        execution_scope: Some(ExecutionScope {
            id: "opaque-scope".to_owned(),
            root: "/workspace".to_owned(),
        }),
        ..Default::default()
    });
    agent.prompt(AgentPromptInput::text("run")).await;
    assert_eq!(received.lock().expect("received").len(), 1);
    assert_eq!(
        received.lock().expect("received")[0].scope.id,
        "opaque-scope"
    );
    assert!(trace.lock().expect("trace").is_empty());

    // Default prepared pipeline records witness separately from public events.
    for mode in [ToolExecutionMode::Parallel, ToolExecutionMode::Sequential] {
        trace.lock().expect("trace").clear();
        let witnesses: Arc<std::sync::Mutex<Vec<ExecutionPipelineWitness>>> = Arc::default();
        let w_clone = witnesses.clone();
        let t_before = trace.clone();
        let t_after = trace.clone();
        let agent2 = Agent::new(AgentOptions {
            initial_state: Some(PartialAgentState {
                model: Some(test_model()),
                tools: Some(vec![read_tool.clone()]),
                ..Default::default()
            }),
            stream_fn: Some(scripted_stream_fn(vec![
                assistant(
                    vec![tool_call("call-1", "read", json!({ "path": "a" }))],
                    StopReason::ToolUse,
                ),
                assistant(vec![text_block("done")], StopReason::Stop),
            ])),
            tool_execution: Some(mode),
            before_tool_call: Some(Arc::new(move |_, _| {
                let t_before = t_before.clone();
                Box::pin(async move {
                    t_before.lock().expect("trace").push("before".to_owned());
                    None
                })
            })),
            after_tool_call: Some(Arc::new(move |_, _| {
                let t_after = t_after.clone();
                Box::pin(async move {
                    t_after.lock().expect("trace").push("after".to_owned());
                    None
                })
            })),
            on_execution_pipeline: Some(Arc::new(move |witness| {
                let w_clone = w_clone.clone();
                Box::pin(async move {
                    w_clone.lock().expect("witnesses").push(witness);
                })
            })),
            ..Default::default()
        });
        agent2.prompt(AgentPromptInput::text("run")).await;
        assert_eq!(
            *trace.lock().expect("trace"),
            vec!["prepare", "before", "body", "after"]
        );
        assert_eq!(
            *witnesses.lock().expect("witnesses"),
            vec![ExecutionPipelineWitness {
                pipeline: "prepared",
                tool_call_id: "call-1".to_owned(),
            }]
        );
    }
}

#[tokio::test]
async fn proxy_message_tool_arguments_reset_replaces_and_extends_raw_prefix() {
    async fn run_sse(frames: &[serde_json::Value]) -> maho_ai::types::AssistantMessage {
        let mut body = String::new();
        for frame in frames {
            body.push_str(&format!("data: {frame}\n\n"));
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        let server = tokio::spawn(async move {
            if let Ok((mut socket, _)) = listener.accept().await {
                let mut req_buf = [0u8; 4096];
                let _ = socket.read(&mut req_buf).await;
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = socket.write_all(resp.as_bytes()).await;
            }
        });
        let stream = stream_proxy(
            &test_model(),
            &maho_ai::types::Context::default(),
            ProxyStreamOptions {
                auth_token: "test-token".to_owned(),
                proxy_url: format!("http://{addr}"),
                ..Default::default()
            },
        );
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), stream.result())
            .await
            .expect("proxy stream finished")
            .expect("proxy result");
        let _ = server.await;
        result
    }

    let empty_reset = run_sse(&[
        json!({ "type": "start" }),
        json!({ "type": "toolcall_start", "contentIndex": 0, "id": "call", "toolName": "read" }),
        json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": "{\"path\":\"a" }),
        json!({ "type": "message_tool_arguments_reset", "contentIndex": 0, "rawArgumentPrefix": "", "parsedThrough": 0 }),
        json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": "{\"path\":\"a.ts\"}" }),
        json!({ "type": "toolcall_end", "contentIndex": 0 }),
        json!({ "type": "done", "reason": "toolUse", "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } } }),
    ])
    .await;
    match empty_reset.content.first() {
        Some(ContentBlock::ToolCall(tc)) => {
            assert_eq!(tc.name, "read");
            assert_eq!(tc.arguments["path"], json!("a.ts"));
        }
        other => panic!("expected toolCall, got {other:?}"),
    }

    let prefix_reset = run_sse(&[
        json!({ "type": "start" }),
        json!({ "type": "toolcall_start", "contentIndex": 0, "id": "call", "toolName": "read" }),
        json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": "{\"path\":\"stale" }),
        json!({ "type": "message_tool_arguments_reset", "contentIndex": 0, "rawArgumentPrefix": "{\"path\":\"b", "parsedThrough": 10 }),
        json!({ "type": "toolcall_delta", "contentIndex": 0, "delta": ".ts\"}" }),
        json!({ "type": "toolcall_end", "contentIndex": 0 }),
        json!({ "type": "done", "reason": "toolUse", "usage": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "totalTokens": 0, "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0, "total": 0 } } }),
    ])
    .await;
    match prefix_reset.content.first() {
        Some(ContentBlock::ToolCall(tc)) => {
            assert_eq!(tc.arguments["path"], json!("b.ts"));
        }
        other => panic!("expected toolCall, got {other:?}"),
    }

    let _ = process_proxy_event;
}

#[tokio::test]
async fn stream_keepalive_extends_silence_window_on_activity_and_caps_keepalive_only_streams() {
    let mut cfg = AgentLoopConfig::new(test_model(), identity_convert_to_llm());
    cfg.options.stream.request.timeout_ms = Some(60);
    cfg.stream_start_timeout_ms = Some(60);
    cfg.stream_keep_alive_timeout_ms = Some(150);

    // Keep-alive activity notes extend the silence window until content arrives.
    let stream_fn_ok: maho_agent::StreamFn = Arc::new(|_, _, options| {
        let on_activity = options.and_then(|o| o.simple.stream.on_transport_activity);
        let stream = AssistantMessageEventStream::assistant();
        let s = stream.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(20));
            for _ in 0..4 {
                interval.tick().await;
                if let Some(cb) = &on_activity {
                    cb();
                }
            }
            s.push(AssistantMessageEvent::Done {
                reason: maho_ai::types::DoneReason::Stop,
                message: assistant(vec![text_block("slow answer")], StopReason::Stop),
            });
        });
        stream
    });

    let messages = run_agent_loop(
        vec![user_message("hi")],
        maho_agent::AgentContext {
            system_prompt: String::new(),
            system_prompt_stable_prefix: None,
            messages: Vec::new(),
            tools: None,
            declared_tools: None,
        },
        cfg.clone(),
        Arc::new(|_| Box::pin(async {})),
        None,
        Some(stream_fn_ok),
    )
    .await
    .expect("keepalive-assisted stream completes");
    assert_eq!(
        messages
            .last()
            .and_then(|m| m.as_assistant())
            .map(|a| a.stop_reason),
        Some(StopReason::Stop)
    );

    // A stream that only sends keep-alives forever hits streamKeepAliveTimeoutMs.
    let mut cfg_cap = cfg;
    cfg_cap.stream_keep_alive_timeout_ms = Some(80);
    let stream_fn_cap: maho_agent::StreamFn = Arc::new(|_, _, options| {
        let on_activity = options.and_then(|o| o.simple.stream.on_transport_activity);
        let stream = AssistantMessageEventStream::assistant();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(20));
            for _ in 0..20 {
                interval.tick().await;
                if let Some(cb) = &on_activity {
                    cb();
                }
            }
        });
        stream
    });

    let capped = run_agent_loop(
        vec![user_message("hi")],
        maho_agent::AgentContext {
            system_prompt: String::new(),
            system_prompt_stable_prefix: None,
            messages: Vec::new(),
            tools: None,
            declared_tools: None,
        },
        cfg_cap,
        Arc::new(|_| Box::pin(async {})),
        None,
        Some(stream_fn_cap),
    )
    .await
    .expect("keepalive-capped returns error assistant message");
    let assistant_err = capped
        .last()
        .and_then(|m| m.as_assistant())
        .expect("assistant message");
    assert_eq!(assistant_err.stop_reason, StopReason::Error);
    assert!(
        assistant_err
            .error_message
            .as_deref()
            .unwrap_or_default()
            .contains("keep-alives")
    );

    let _ = StreamError::new("unused");
}
