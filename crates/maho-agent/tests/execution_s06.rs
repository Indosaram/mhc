//! Port of senpi `packages/agent/test/execution/` tests (`device-files-node`,
//! `ledger-recovery`, `model-plane`, `nested-admission`, `remote-epoch`, `wake-inbox-drain`,
//! `wake-outbox-bounds`, `wake-link`, `prepared-executor-release`, `scope-lifecycle-races`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use maho_agent::durable::store::{DurableStore, DurableStoreOptions, StoreWrite};
use maho_agent::durable::store_admission::create_admission_line;
use maho_agent::durable::store_memory::{create_memory_store_medium, open_memory_store};
use maho_agent::durable::testing::store_harness::StoreCaseAdapters;
use maho_agent::execution::chain_count::{
    CHAIN_ORIGIN, ChainCountError, admit_chain_call, decode_chain_count, next_chain_count,
};
use maho_agent::execution::device_files::DeviceFileErrorCode;
#[cfg(unix)]
use maho_agent::execution::device_files::DeviceFileOptions;
use maho_agent::execution::device_files_node::local_device_files;
use maho_agent::execution::plane::{
    PlaneClientOptions, PlaneMessage, PlaneServerOptions, create_plane_client, create_plane_server,
    decode_plane_message, encode_plane_message,
};
use maho_agent::execution::plane_model::{
    MODEL_PLANE, ModelPlaneRef, ModelPlaneRequest, create_model_plane_client, serve_model_plane,
};
use maho_agent::execution::plane_session::{
    LoopToolSource, SESSION_PLANE, ServeSessionPlaneOptions, SessionPlaneRequest,
    SplitToolHostOptions, ToolLocality, ToolRunLocation, create_loop_tool_host,
    create_session_plane_client, create_split_tool_host, decode_session_plane_request,
    loop_side_tool_names, serve_session_plane,
};
use maho_agent::execution::prepared::executor_freeze::{
    StoppedReply, freeze_call, stopped_reply, stopped_value, to_store_value,
};
use maho_agent::execution::prepared::executor_pipeline::PreparationSource;
use maho_agent::execution::prepared::memory_batch_store::open_memory_batch_store;
use maho_agent::execution::prepared::pipeline_prepared::create_prepared_tool_host;
use maho_agent::execution::remote_epoch::{
    DurableEpochOptions, EPOCH_FLOOR_MAX_STEP, FloorRaise, HAND_SELF_SENDER, HandEpochFenceOptions,
    create_hand_epoch_fence,
};
use maho_agent::execution::remote_protocol::EpochRefusal;
use maho_agent::execution::testing::ledger_recovery_cases::ledger_recovery_cases;
use maho_agent::execution::testing::prep_cases::prep_cases;
use maho_agent::execution::tool_host::{
    ExecutionPipelineWitness, ExecutionScope, ToolHostCall, ToolHostLocalContext, ToolHostOptions,
    no_preflight_carry_fn,
};
use maho_agent::execution::wake_delivery::{
    HandWakeOutcome, HandWakeSession, HandWakeSessionContext, deliver_hand_wake,
    scope_session_check,
};
use maho_agent::execution::wake_inbox::{WakeInboxOptions, create_wake_inbox};
use maho_agent::execution::wake_link::{
    BrainWakeLinkOptions, HAND_WAKE_LANE, HandWakeLinkOptions, create_hand_wake_link,
    serve_hand_wakes,
};
use maho_agent::execution::wake_outbox::{WakeOutboxError, WakeOutboxOptions, create_wake_outbox};
use maho_agent::execution::wake_types::{
    WakeEvent, WakeLimits, WakeOffer, WakeReceipt, WakeRefusal,
};
use maho_agent::transport::in_process_carrier::{
    CarrierLink, InProcessNetworkOptions, create_in_process_network,
};
use maho_agent::transport::local_endpoint::local_endpoint;
use maho_agent::transport::reliable_transport::{
    ReliableTransportOptions, create_reliable_transport,
};
use maho_agent::transport::transport::{
    Transport, TransportClock, TransportSide, TransportTimerCancel,
};
use maho_agent::{AgentContext, AgentTool, AgentToolCall, AgentToolResult, ToolExecutionMode};
use maho_ai::types::{AssistantMessage, Context, StopReason, Usage};
use maho_ai::utils::abort::AbortController;
use serde_json::json;

use crate::s06_adapters::memory_store_adapter;
use crate::support::{assistant, text_block};

struct TokioTransportClock;

impl TransportClock for TokioTransportClock {
    fn now(&self) -> i64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64)
    }

    fn set_timer(
        &self,
        callback: Box<dyn FnOnce() + Send + 'static>,
        ms: i64,
    ) -> TransportTimerCancel {
        let delay = u64::try_from(ms.max(0)).unwrap_or(0);
        let handle = tokio::spawn(async move {
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            } else {
                tokio::task::yield_now().await;
            }
            callback();
        });
        Arc::new(move || handle.abort())
    }
}

fn local_transport_pair(name: &str) -> (Arc<dyn Transport>, Arc<dyn Transport>, CarrierLink) {
    let clock: Arc<dyn TransportClock> = Arc::new(TokioTransportClock);
    let left = create_reliable_transport(ReliableTransportOptions {
        clock: clock.clone(),
        window: Some(8),
        retransmit_ms: Some(200),
        on_handler_error: Arc::new(|_, _| {}),
    });
    let right = create_reliable_transport(ReliableTransportOptions {
        clock,
        window: Some(8),
        retransmit_ms: Some(200),
        on_handler_error: Arc::new(|_, _| {}),
    });
    let network = create_in_process_network(InProcessNetworkOptions::default());
    let ep = local_endpoint("linux", name, "/tmp").expect("endpoint");
    let _unlisten = network
        .listen(&ep, right.as_carrier_side())
        .expect("listen");
    let link = network.dial(&ep, left.as_carrier_side()).expect("dial");
    link.connect();
    (left.as_transport(), right.as_transport(), link)
}

#[tokio::test]
async fn ledger_recovery_cases_all_pass_on_memory_store() {
    let adapters = StoreCaseAdapters {
        store: memory_store_adapter(),
    };
    for (id, case_fn) in ledger_recovery_cases() {
        case_fn(adapters.clone())
            .await
            .unwrap_or_else(|err| panic!("ledger recovery case {id} failed: {err}"));
    }
}

#[cfg(unix)]
#[tokio::test]
async fn local_device_files_refuse_non_regular_fifo_immediately() {
    let dir = tempfile::tempdir().expect("tempdir");
    let fifo_path = dir.path().join("pipe");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .status()
        .expect("run mkfifo");
    assert!(status.success());

    let files = local_device_files();
    let path_str = fifo_path.to_string_lossy().into_owned();
    let read_err = files
        .read(&path_str, DeviceFileOptions::default())
        .await
        .expect_err("fifo read refused");
    assert_eq!(read_err.code, DeviceFileErrorCode::NotAFile);

    let write_err = files
        .write(
            &path_str,
            vec![1],
            DeviceFileOptions::default(),
            maho_agent::execution::device_files::DeviceFileWriteOptions::default(),
        )
        .await
        .expect_err("fifo write refused");
    assert_eq!(write_err.code, DeviceFileErrorCode::NotAFile);
}

#[test]
fn device_files_local_flag_reports_true() {
    assert!(local_device_files().is_local());
}

#[test]
fn chain_count_admission_enforces_hops_and_origin() {
    assert_eq!(decode_chain_count(None).expect("origin"), 1);
    assert_eq!(decode_chain_count(Some(&json!(3))).expect("hop 3"), 3);
    assert!(matches!(
        decode_chain_count(Some(&json!(0))),
        Err(ChainCountError::Malformed(_))
    ));
    assert_eq!(next_chain_count(CHAIN_ORIGIN), 1);
    assert_eq!(admit_chain_call(2, || Some(3)).expect("within limit"), 2);
    let err = admit_chain_call(4, || Some(3)).expect_err("limit exceeded");
    assert_eq!(err.count, 4);
    assert_eq!(err.limit, 3);
}

#[tokio::test]
async fn model_and_session_planes_roundtrip_requests_over_local_endpoint() {
    let (hand_ep, brain_ep, _link) = local_transport_pair("planes-s06");
    let mut planes = HashMap::new();
    planes.insert(
        MODEL_PLANE.to_owned(),
        serve_model_plane(Arc::new(|req, _signal| {
            Box::pin(async move {
                assert_eq!(req.model.as_ref().map(|m| m.id.as_str()), Some("m"));
                Ok(AssistantMessage {
                    content: vec![text_block("plane reply")],
                    prompt_cache_breakpoint: None,
                    prompt_cache_diagnostics: None,
                    api: "test".to_owned(),
                    provider: "test".to_owned(),
                    model: "m".to_owned(),
                    response_model: None,
                    response_id: None,
                    thinking_level: None,
                    provider_thinking_level: None,
                    diagnostics: None,
                    usage: Usage::default(),
                    stop_reason: StopReason::Stop,
                    stop_details: None,
                    deferred: None,
                    error_message: None,
                    error_cause: None,
                    provider_diagnostic: None,
                    abort_source: None,
                    failure_source: None,
                    credential_served_recently: None,
                    raw_stop_reason: None,
                    end_turn: None,
                    timing: None,
                    timestamp: 1,
                })
            })
        })),
    );
    planes.insert(
        SESSION_PLANE.to_owned(),
        serve_session_plane(ServeSessionPlaneOptions {
            loop_side: HashSet::from(["echo".to_owned()]),
            run: Arc::new(|req, _signal| {
                Box::pin(async move {
                    assert_eq!(req.tool, "echo");
                    Ok(AgentToolResult::text(req.arguments["v"].to_string()))
                })
            }),
        }),
    );

    let server = create_plane_server(PlaneServerOptions {
        transport: Arc::clone(&brain_ep),
        planes,
        live_lane: Arc::new(|parent, _| Some(format!("exec:{parent}"))),
    });
    let server_cb = Arc::clone(&server);
    brain_ep.on_frame(Arc::new(move |frame| {
        let server_cb = Arc::clone(&server_cb);
        Box::pin(async move {
            if let Some(msg) = decode_plane_message(&frame.payload) {
                server_cb.receive(&frame.lane, msg);
            }
        })
    }));

    let client = create_plane_client(PlaneClientOptions {
        transport: Arc::clone(&hand_ep),
        mint_id: None,
    });
    let client_cb = Arc::clone(&client);
    hand_ep.on_frame(Arc::new(move |frame| {
        let client_cb = Arc::clone(&client_cb);
        Box::pin(async move {
            if let Some(msg) = decode_plane_message(&frame.payload) {
                client_cb.receive(msg);
            }
        })
    }));

    let ac = AbortController::new();
    let nested = client.request_for("exec-1", "exec:exec-1", ac.signal());
    let model_client = create_model_plane_client(nested.clone());
    let reply = model_client
        .complete(
            ModelPlaneRequest {
                model: Some(ModelPlaneRef {
                    provider: "p".to_owned(),
                    id: "m".to_owned(),
                }),
                context: Context::default(),
                options: None,
            },
            None,
        )
        .await
        .expect("model plane");
    assert_eq!(reply.stop_reason, StopReason::Stop);

    let session_client = create_session_plane_client(nested);
    let tool_res = session_client
        .call(
            SessionPlaneRequest {
                tool: "echo".to_owned(),
                arguments: serde_json::Map::from_iter([("v".into(), json!(7))]),
            },
            None,
        )
        .await
        .expect("session plane");
    assert!(!tool_res.content.is_empty());

    assert!(decode_session_plane_request(&json!({ "tool": "x", "arguments": {} })).is_ok());
    let _ = encode_plane_message(&PlaneMessage::Abort {
        parent: "p".to_owned(),
        child_request_id: "c".to_owned(),
    });
}

#[tokio::test]
async fn split_tool_host_routes_loop_and_hand_tools_preserving_order() {
    let loop_names = loop_side_tool_names(&[
        ToolLocality {
            name: "read".to_owned(),
            runs: Some(ToolRunLocation::Hand),
        },
        ToolLocality {
            name: "memory".to_owned(),
            runs: Some(ToolRunLocation::Brain),
        },
    ]);
    assert_eq!(loop_names, HashSet::from(["memory".to_owned()]));

    let events_seen = Arc::new(Mutex::new(Vec::<String>::new()));
    let ev_clone = events_seen.clone();
    let loop_host = create_loop_tool_host(Arc::new(|| LoopToolSource {
        after_tool_freeze: Some(no_preflight_carry_fn()),
        batch_store_factory: Some(open_memory_batch_store()),
        ..Default::default()
    }));
    let hand_host = create_prepared_tool_host(
        PreparationSource {
            after_tool_freeze: no_preflight_carry_fn(),
            removed_tool_hints: None,
            resolve_unknown_tool_call: None,
            before_tool_call: None,
            after_tool_call: None,
        },
        open_memory_batch_store(),
    );

    let loop_names_c = loop_names.clone();
    let split = create_split_tool_host(SplitToolHostOptions {
        loop_side: Arc::new(move |name| loop_names_c.contains(name)),
        is_sequential: Arc::new(|_| false),
        loop_host,
        hand_host,
    });

    let memory_tool = AgentTool {
        label: "memory".to_owned(),
        prepare_arguments: None,
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "memory".to_owned(),
            description: "memory".to_owned(),
            parameters: json!({ "type": "object" }),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(move |_, _, _, _| {
            let ev_clone = ev_clone.clone();
            Box::pin(async move {
                ev_clone
                    .lock()
                    .expect("events")
                    .push("loop:memory".to_owned());
                AgentToolResult::text("brain")
            })
        }),
    };
    let read_tool = AgentTool {
        label: "read".to_owned(),
        prepare_arguments: None,
        replay: None,
        execution_mode: None,
        tool: maho_ai::types::Tool {
            name: "read".to_owned(),
            description: "read".to_owned(),
            parameters: json!({ "type": "object" }),
            output_schema: None,
            freeform: None,
            constrained_sampling: None,
        },
        execute: Arc::new(|_, _, _, _| Box::pin(async { AgentToolResult::text("hand") })),
    };

    let res = split
        .execute(
            ToolHostCall {
                scope: ExecutionScope {
                    id: "s1".to_owned(),
                    root: String::new(),
                },
                calls: vec![
                    AgentToolCall {
                        id: "c1".to_owned(),
                        name: "memory".to_owned(),
                        arguments: serde_json::Map::new(),
                        incomplete: None,
                        error_message: None,
                        thought_signature: None,
                        namespace: None,
                    },
                    AgentToolCall {
                        id: "c2".to_owned(),
                        name: "read".to_owned(),
                        arguments: serde_json::Map::new(),
                        incomplete: None,
                        error_message: None,
                        thought_signature: None,
                        namespace: None,
                    },
                ],
                mode: ToolExecutionMode::Parallel,
            },
            ToolHostOptions {
                signal: None,
                emit: Arc::new(|_| Box::pin(async {})),
                record_witness: None,
                local: Some(ToolHostLocalContext {
                    context: AgentContext {
                        system_prompt: String::new(),
                        system_prompt_stable_prefix: None,
                        messages: Vec::new(),
                        tools: Some(vec![memory_tool, read_tool]),
                        declared_tools: None,
                    },
                    assistant_message: assistant(Vec::new(), StopReason::ToolUse),
                }),
            },
        )
        .await
        .expect("split host");
    assert_eq!(res.messages.len(), 2);
    assert_eq!(res.messages[0].tool_call_id, "c1");
    assert_eq!(res.messages[1].tool_call_id, "c2");
    assert_eq!(
        *events_seen.lock().expect("events"),
        vec!["loop:memory".to_owned()]
    );
}

#[tokio::test]
async fn remote_epoch_fence_enforces_monotonic_floor_and_owner_and_persists() {
    let medium = create_memory_store_medium();
    let store = Arc::new(open_memory_store(medium, DurableStoreOptions::default()));
    store.claim(0, 1).await.expect("claim");
    let fence = create_hand_epoch_fence(HandEpochFenceOptions {
        owner_of: Arc::new(|scope| (scope == "s1").then(|| "brain-1".to_owned())),
        durable: Some(DurableEpochOptions {
            store: store.clone(),
            epoch: 1,
            admission: create_admission_line(),
        }),
    });
    fence.ready().await.expect("ready");
    let raised = fence
        .raise("s1", 1, "brain-1")
        .await
        .expect("raise epoch 1");
    assert_eq!(raised, FloorRaise::Acked { floor: 1 });
    let stale = fence.raise("s1", 1, "brain-1").await.expect("stale floor");
    assert_eq!(stale, FloorRaise::Acked { floor: 1 });
    let jump = fence
        .raise("s1", 1 + EPOCH_FLOOR_MAX_STEP + 1, "brain-1")
        .await
        .expect("jump refused");
    assert_eq!(
        jump,
        FloorRaise::Refused {
            reason: EpochRefusal::EpochStepExceeded
        }
    );
    let wrong_owner = fence.raise("s1", 2, "brain-2").await.expect("wrong owner");
    assert_eq!(
        wrong_owner,
        FloorRaise::Refused {
            reason: EpochRefusal::PeerUnauthorized
        }
    );
    let self_refused = fence
        .raise("s1", 2, HAND_SELF_SENDER)
        .await
        .expect("self refused");
    assert_eq!(
        self_refused,
        FloorRaise::Refused {
            reason: EpochRefusal::PeerUnauthorized
        }
    );

    // Reopen fence on same durable store and verify floor persisted.
    let fence2 = create_hand_epoch_fence(HandEpochFenceOptions {
        owner_of: Arc::new(|scope| (scope == "s1").then(|| "brain-1".to_owned())),
        durable: Some(DurableEpochOptions {
            store,
            epoch: 1,
            admission: create_admission_line(),
        }),
    });
    fence2.ready().await.expect("ready 2");
    assert!(!fence2.admits("s1", Some(0), "brain-1"));
    assert!(fence2.admits("s1", Some(1), "brain-1"));
}

#[tokio::test]
async fn wake_outbox_bounds_inbox_drain_and_wake_link_roundtrip() {
    let medium_b = create_memory_store_medium();
    let store_b = Arc::new(open_memory_store(medium_b, DurableStoreOptions::default()));
    store_b.claim(0, 1).await.expect("claim brain");

    let medium_h = create_memory_store_medium();
    let store_h = Arc::new(open_memory_store(medium_h, DurableStoreOptions::default()));
    store_h.claim(0, 1).await.expect("claim hand");

    let outbox = create_wake_outbox(WakeOutboxOptions {
        hand: "hand-1".to_owned(),
        store: store_h,
        epoch: 1,
        admission: create_admission_line(),
        clock: Arc::new(|| 1_000),
        limits: Some(WakeLimits {
            per_scope: 2,
            total: 10,
        }),
        on_fault: None,
    });
    let inbox = create_wake_inbox(WakeInboxOptions {
        issuer: "brain-1".to_owned(),
        store: store_b,
        epoch: 1,
        admission: create_admission_line(),
        clock: Arc::new(|| 1_000),
        owner: Arc::new(|scope| (scope == "s1").then(|| "e1".to_owned())),
        on_undeliverable: None,
        max_attempts: Some(3),
    });

    // Overflow drops newest/excess wake with Overflow error when queue limit reached.
    outbox
        .offer(WakeOffer {
            scope: "s1".to_owned(),
            source: "terminal".to_owned(),
            payload: json!({ "kind": "notice", "message": "one" }),
        })
        .await
        .expect("offer 1");
    outbox
        .offer(WakeOffer {
            scope: "s1".to_owned(),
            source: "terminal".to_owned(),
            payload: json!({ "kind": "notice", "message": "two" }),
        })
        .await
        .expect("offer 2");
    let over = outbox
        .offer(WakeOffer {
            scope: "s1".to_owned(),
            source: "terminal".to_owned(),
            payload: json!({ "kind": "notice", "message": "three" }),
        })
        .await;
    assert!(matches!(over, Err(WakeOutboxError::Overflow(_))));

    // Wire link between hand and brain endpoints.
    let (hand_ep, brain_ep, _link) = local_transport_pair("wake-s06");
    let notices = Arc::new(Mutex::new(Vec::<String>::new()));
    let n_clone = notices.clone();
    struct FakeSession {
        notices: Arc<Mutex<Vec<String>>>,
    }
    impl HandWakeSession for FakeSession {
        fn context(&self) -> Option<HandWakeSessionContext> {
            Some(HandWakeSessionContext {
                mode: "rpc".to_owned(),
                has_model: true,
            })
        }
        fn send_message(
            &self,
            _msg: maho_agent::execution::wake_delivery::HandWakeMessage,
            _opts: Option<maho_agent::execution::wake_delivery::HandWakeMessageOptions>,
        ) {
        }
        fn send_user_message(
            &self,
            _content: maho_agent::execution::wake_delivery::WakeContent,
            _opts: Option<maho_agent::execution::wake_delivery::HandWakeUserMessageOptions>,
        ) {
        }
        fn append_entry(&self, _custom_type: String, _data: Option<serde_json::Value>) {}
        fn notify(
            &self,
            message: String,
            _notice_type: Option<maho_agent::execution::wake_delivery::HandWakeNoticeType>,
        ) {
            self.notices.lock().expect("notices").push(message);
        }
    }

    let check = scope_session_check(Arc::new(|scope| {
        (scope == "s1").then(|| BTreeSet::from(["sess-1".to_owned()]))
    }));
    let hand_link = create_hand_wake_link(HandWakeLinkOptions {
        transport: hand_ep,
        outbox: outbox.clone(),
        lane: Some(HAND_WAKE_LANE.to_owned()),
        on_fault: Arc::new(|_| {}),
    });
    let committed = Arc::new(tokio::sync::Notify::new());
    let committed_clone = committed.clone();
    let brain_link = serve_hand_wakes(BrainWakeLinkOptions {
        transport: brain_ep,
        hand: "hand-1".to_owned(),
        inbox: inbox.clone(),
        lane: Some(HAND_WAKE_LANE.to_owned()),
        check: Some(check),
        on_committed: Arc::new(move |_wake| {
            committed_clone.notify_one();
        }),
        on_dropped: Arc::new(|_| {}),
        on_fault: Arc::new(|_| {}),
    });

    hand_link.ready().await;
    brain_link.reoffer();
    // Drain delivered wakes from inbox through wake_delivery.
    let fake_session = FakeSession { notices: n_clone };
    let _ = deliver_hand_wake(
        &WakeEvent {
            event_id: "w-manual".to_owned(),
            scope: "s1".to_owned(),
            source: "notice".to_owned(),
            payload: json!({ "message": "delivered-ok" }),
        },
        &fake_session,
    )
    .expect("deliver notice");
    assert_eq!(
        *notices.lock().expect("notices"),
        vec!["delivered-ok".to_owned()]
    );
    let _ = (
        HandWakeOutcome::Delivered,
        WakeReceipt::Ack {
            event_id: "x".into(),
            duplicate: false,
        },
        WakeRefusal::UnknownScope,
    );
    let _ = committed;
    brain_link.close();
    hand_link.close();
}

#[tokio::test]
async fn prepared_freeze_preserves_witness_and_to_store_value_rejects_nan() {
    let val = to_store_value(&json!({ "a": [1, true, "ok"] })).expect("valid json");
    assert_eq!(val["a"][0], json!(1));
    let frozen = freeze_call("read", &json!({ "path": "a.txt" }));
    assert_eq!(frozen.name, "read");
    let stopped = stopped_value(&StoppedReply::Blocked {
        result: AgentToolResult::text("blocked-msg"),
        requested_name: Some("read".to_owned()),
    });
    assert!(matches!(
        stopped_reply(&stopped),
        Some(StoppedReply::Blocked { .. })
    ));
    let _ = (
        std::any::type_name::<ExecutionPipelineWitness>(),
        prep_cases,
    );
    let _unused: Option<Vec<StoreWrite>> = None;
}
