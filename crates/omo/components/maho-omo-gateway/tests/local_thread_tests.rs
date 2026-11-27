//! Integration tests for Package P10c (O01 local thread tools, cold wake, host-ensure, call budget).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use maho_ext_api::{
    EventBus, Extension, ExtensionApi, ExtensionRuntime, ExtensionSessionProfile, ExtensionSide,
    LoadedExtension, SourceInfo, ToolExposure,
};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::{Notify, mpsc, oneshot};

use maho_omo_gateway::call_budget::call_deadline::{
    READ_RESERVE_MS, TOOL_CALL_BUDGET_MS, command_wait_ms, outside_call_deadline,
    wait_within_call_budget, with_call_deadline, within_call_budget,
};
use maho_omo_gateway::thread::address::{
    ResolveContext, ResolveOptions, ResolveWhere, resolve_address,
};
use maho_omo_gateway::thread::address_core::{
    NamedRow, ParsedAddress, normalize_name, parse_address, row_name,
};
use maho_omo_gateway::thread::addressing::ThreadAddressEntry;
use maho_omo_gateway::thread::answer_shape::{AnswerShape, UiRequestKind, answer_shape};
use maho_omo_gateway::thread::attachment_fence::{FenceOutcome, RpcTransport, fenced_command};
use maho_omo_gateway::thread::contracts::params::{ThreadCreateInput, ThreadRenameInput};
use maho_omo_gateway::thread::contracts::results::{
    ThreadInterruptBackground, ThreadStatus, ThreadSummary, ThreadWaitOutcome,
};
use maho_omo_gateway::thread::contracts::state::ThreadActivity;
use maho_omo_gateway::thread::control::close_ladder::{RecordAnswer, close_ladder};
use maho_omo_gateway::thread::control::create_launch::create_launch;
use maho_omo_gateway::thread::control::operations::{close_thread, set_model_on};
use maho_omo_gateway::thread::control::port::{
    AvailableModels, ControlError, SessionControl, SessionModelState, refuse,
};
use maho_omo_gateway::thread::control::rename::rename_thread;
use maho_omo_gateway::thread::control::wait_settled::wait_for_turn_end;
use maho_omo_gateway::thread::live_surface::{
    HostEndpointReport, LiveThreadSurface, LiveThreadSurfaceOptions, UnixJsonlTransport,
    parse_host_status_all,
};
use maho_omo_gateway::thread::metadata::THREAD_FAMILY_PROMPT_GUIDELINES;
use maho_omo_gateway::thread::native::NativeThreadComponent;
use maho_omo_gateway::thread::paged_reader::{
    ContextTail, EntryPageRead, TailMessage, is_entry_cursor, select_entry_page,
};
use maho_omo_gateway::thread::reader::ThreadTranscriptEntry;
use maho_omo_gateway::thread::schema::GATEWAY_MIGRATIONS;
use maho_omo_gateway::thread::store::{
    GatewayStoreOptions, SqliteGatewayStore, create_gateway_store,
};
use maho_omo_gateway::thread::store_api::GatewayStoreApi;
use maho_omo_gateway::thread::tools::build_local_thread_tools;
use maho_omo_gateway::thread::tools::model_match::{ModelChoice, ModelMatchResult, match_model};
use maho_omo_gateway::thread::tools::ports::{
    BoxFuture, SessionWatch, SessionWatchInitial, SessionWatchOutcome, ThreadCloseAnswer,
    ThreadCloseOptions, ThreadCloseOutcome, ThreadHost, ThreadHostSession, ThreadHostView,
    ThreadToolSurfaceOptions,
};
use maho_omo_gateway::thread::tools::reply_permission::{ReplySender, thread_send_permission};
use maho_omo_gateway::thread::tools::thread_state::thread_state;
use maho_omo_gateway::thread::types::{
    EnqueueOutcome, EnqueueRequest, EnvelopeOrigin, GatewayDeliveryMode, GatewayEndpointKind,
    Provenance, ReceiptRef,
};
use maho_omo_gateway::thread::wake::carrier::{Carrier, CarrierOptions, CarrierWakeOutcome};
use maho_omo_gateway::thread::wake::carrier_connection::CarrierHostConnection;
use maho_omo_gateway::thread::wake::engine_minimum::{
    MIN_WAKE_ENGINE_VERSION, engine_drops_retained_record,
};
use maho_omo_gateway::thread::wake::host_ensure::parse_host_ensure_json;
use maho_omo_gateway::thread::wake::session_target::{
    WakeSessionKind, WakeTarget, resolve_wake_target,
};
use maho_omo_gateway::thread::woken_types::{WokenSessionRow, WokenSessionState};
use maho_omo_gateway::thread::worktree::discard_worktree::DiscardWorktreeOptions;
use maho_omo_gateway::thread::worktree::{
    WorktreeOutcome, WorktreeRequest, create_thread_worktree, discard_worktree, run_git,
};

fn test_store(dir: &Path, now_ms: i64) -> Arc<SqliteGatewayStore> {
    let mut opts = GatewayStoreOptions::new(dir.to_string_lossy().into_owned());
    opts.now = Some(Arc::new(move || now_ms));
    create_gateway_store(opts).expect("create gateway store")
}

#[test]
fn expired_causal_root_refuses_followup_delivery() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = test_store(tmp.path(), 10_000);
    let mut request = EnqueueRequest {
        provenance: Provenance {
            workspace: "ws".into(),
            project: "proj".into(),
            lane: None,
            session: None,
            device: None,
            device_absent: None,
        },
        now: 10_000,
        delivery_id: "original".into(),
        target_durable_id: "worker-1".into(),
        sender_principal: "session:caller".into(),
        rate_principal: None,
        sender_node: "caller".into(),
        sender_turn: None,
        cause_delivery_id: None,
        inbound_hop: None,
        authority: None,
        claimed_root_id: None,
        origin: EnvelopeOrigin::Session {
            session: "caller".into(),
        },
        actor: "Lead".into(),
        body: "original".into(),
        mode: GatewayDeliveryMode::Auto,
        expected_turn_id: None,
        binding: None,
        receipt: ReceiptRef {
            idempotency_key: "original-key".into(),
            args_hash: "original-hash".into(),
        },
        endpoint_kind: None,
        collapse_key: None,
    };
    let first = store.enqueue(&request).expect("initial enqueue");
    let EnqueueOutcome::Inserted { row, .. } = first else {
        panic!("initial delivery was not inserted: {first:?}");
    };
    let root_id = row.root_id.clone();
    let claim = store
        .claim(&maho_omo_gateway::thread::types::ClaimRequest {
            now: request.now,
            delivery_id: request.delivery_id.clone(),
            self_identity: maho_omo_gateway::thread::types::ProcessIdentity {
                pid: 1,
                process_start_time: None,
                instance_id: "test".into(),
                runtime_instance: None,
            },
            lane: maho_omo_gateway::thread::types::EffectiveLane::Start,
        })
        .expect("claim initial delivery");
    assert!(matches!(
        claim,
        maho_omo_gateway::thread::types::ClaimOutcome::Claimed { .. }
    ));
    request.now += maho_omo_gateway::thread::constants::ROOT_LIFETIME_MS;
    request.delivery_id = "followup".into();
    request.target_durable_id = "worker-2".into();
    request.sender_node = "worker-1".into();
    request.cause_delivery_id = Some("original".into());
    request.receipt.idempotency_key = "followup-key".into();
    request.receipt.args_hash = "followup-hash".into();
    let outcome = store.enqueue(&request).expect("followup enqueue");
    let EnqueueOutcome::Refused { code, details, .. } = outcome else {
        panic!("expired root should be refused: {outcome:?}");
    };
    assert_eq!(code.as_str(), "loop_detected");
    let details = details.expect("refusal details");
    assert_eq!(details["guard"], "root_expired");
    assert_eq!(details["root_id"], root_id);
}

// ---------------------------------------------------------------------------
// 1. Call budget (#14130)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn call_deadline_bounds_nested_waits_and_respects_outside_scope() {
    assert_eq!(TOOL_CALL_BUDGET_MS, 50_000);
    assert_eq!(READ_RESERVE_MS, 15_000);

    // Without active deadline, bounds pass through untouched.
    assert_eq!(command_wait_ms(30_000, Some(1_000)), 30_000);
    assert_eq!(wait_within_call_budget(25_000, None, Some(1_000)), 25_000);

    let result = with_call_deadline(100_000, async {
        // 40s left, reserve 15s => 25s allowed.
        assert_eq!(command_wait_ms(60_000, Some(60_000)), 40_000);
        assert_eq!(wait_within_call_budget(30_000, None, Some(60_000)), 25_000);
        // Only 10s left < 15s reserve => 0ms wait allowed.
        assert_eq!(wait_within_call_budget(20_000, None, Some(90_000)), 0);

        // outside_call_deadline detaches from enclosing call deadline.
        let detached = outside_call_deadline(async {
            assert_eq!(command_wait_ms(60_000, Some(95_000)), 60_000);
            42
        })
        .await;
        assert_eq!(detached, 42);

        // within_call_budget returns None immediately when deadline <= now.
        let expired = within_call_budget(std::future::pending::<i32>(), Some(100_000)).await;
        assert_eq!(expired, None);

        within_call_budget(async { 7 }, Some(50_000)).await
    })
    .await;

    assert_eq!(result, Some(7));
}

// ---------------------------------------------------------------------------
// 2. SQLite schema v11..v16 + backlog + collapse_key + answer_shape
// ---------------------------------------------------------------------------

#[test]
fn gateway_store_migrates_to_v16_handles_backlog_collapse_and_answer_shape() {
    let tmp = tempfile::tempdir().expect("tempdir");
    assert_eq!(GATEWAY_MIGRATIONS.len(), 16);

    let store = test_store(tmp.path(), 10_000);

    let prov = Provenance {
        workspace: "ws".into(),
        project: "proj".into(),
        lane: None,
        session: None,
        device: None,
        device_absent: None,
    };

    // Enqueue two deliveries with the same collapse_key: second replaces first while queued.
    let first = store
        .enqueue(&EnqueueRequest {
            provenance: prov.clone(),
            now: 10_000,
            delivery_id: "del-1".into(),
            target_durable_id: "worker-1".into(),
            sender_principal: "session:caller-1".into(),
            rate_principal: None,
            sender_node: "session:caller-1".into(),
            sender_turn: None,
            cause_delivery_id: None,
            inbound_hop: None,
            authority: None,
            claimed_root_id: None,
            origin: EnvelopeOrigin::Session {
                session: "caller-1".into(),
            },
            actor: "Lead".into(),
            body: "first draft".into(),
            mode: GatewayDeliveryMode::Auto,
            expected_turn_id: None,
            binding: None,
            receipt: ReceiptRef {
                idempotency_key: "k-1".into(),
                args_hash: "h-1".into(),
            },
            endpoint_kind: None,
            collapse_key: Some("status-sync".into()),
        })
        .expect("first enqueue");
    assert!(matches!(first, EnqueueOutcome::Inserted { .. }));

    let second = store
        .enqueue(&EnqueueRequest {
            provenance: prov,
            now: 10_001,
            delivery_id: "del-2".into(),
            target_durable_id: "worker-1".into(),
            sender_principal: "session:caller-1".into(),
            rate_principal: None,
            sender_node: "session:caller-1".into(),
            sender_turn: None,
            cause_delivery_id: None,
            inbound_hop: None,
            authority: None,
            claimed_root_id: None,
            origin: EnvelopeOrigin::Session {
                session: "caller-1".into(),
            },
            actor: "Lead".into(),
            body: "second draft".into(),
            mode: GatewayDeliveryMode::Auto,
            expected_turn_id: None,
            binding: None,
            receipt: ReceiptRef {
                idempotency_key: "k-2".into(),
                args_hash: "h-2".into(),
            },
            endpoint_kind: None,
            collapse_key: Some("status-sync".into()),
        })
        .expect("second enqueue");
    assert!(
        matches!(second, EnqueueOutcome::Inserted { .. }),
        "second={second:?}"
    );

    // Backlog counts 1 live waiting delivery for worker-1.
    let backlogs = store.backlogs(&["worker-1".into()]).expect("backlog query");
    assert_eq!(backlogs.get("worker-1").map(|b| b.held), Some(1));

    // Answer shaping covers confirm/select/question + blank refusal.
    assert_eq!(
        answer_shape(Some(UiRequestKind::Confirm), "YES"),
        AnswerShape::Ok(json!({ "confirmed": true }))
    );
    assert_eq!(
        answer_shape(Some(UiRequestKind::Select), "beta"),
        AnswerShape::Ok(json!({ "value": "beta" }))
    );
    assert!(matches!(
        answer_shape(None, " \u{200b} "),
        AnswerShape::Refused(_)
    ));
}

// ---------------------------------------------------------------------------
// 3. Addressing + local resolver + O02 typed refusals
// ---------------------------------------------------------------------------

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn git_workspace_lookup_keeps_runtime_responsive() {
    use std::io::{Read, Write};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let mode = std::env::var("MHC_GIT_TEST_MODE").unwrap_or_default();
    if mode == "git" {
        let mut stream =
            std::net::TcpStream::connect(std::env::var("MHC_GIT_TEST_ADDRESS").unwrap()).unwrap();
        stream.write_all(b"G").unwrap();
        let mut release = [0; 1];
        stream.read_exact(&mut release).unwrap();
        println!("/repo");
        return;
    }
    if mode == "worker" {
        let mut control =
            tokio::net::TcpStream::connect(std::env::var("MHC_GIT_TEST_ADDRESS").unwrap())
                .await
                .unwrap();
        control.write_all(b"C").await.unwrap();
        let entry = ThreadAddressEntry::from_summary(ThreadSummary {
            thread_id: "worker".into(),
            name: "worker".into(),
            alias: None,
            cwd: "/repo".into(),
            status: ThreadStatus::Live,
            created_at: "2026-11-24T00:00:00Z".into(),
            updated_at: None,
            error_note: None,
            endpoint: None,
            surface: None,
            alive: Some(true),
            controls: None,
            skills: None,
            device: None,
        });
        let lookup = tokio::spawn(async move {
            let ctx = ResolveContext::local_only("caller", "/repo", vec![entry]);
            resolve_address(&ctx, "worker", &ResolveOptions::default()).await
        });
        let mut probe = [0; 1];
        control.read_exact(&mut probe).await.unwrap();
        control.write_all(b"T").await.unwrap();
        assert_eq!(lookup.await.unwrap().unwrap().thread, "worker");
        return;
    }

    let tmp = tempfile::tempdir().unwrap();
    let git = tmp.path().join("git");
    std::fs::write(
        &git,
        "#!/bin/sh\nMHC_GIT_TEST_MODE=git exec \"$MHC_GIT_TEST_EXE\" --exact local_thread_tests::git_workspace_lookup_keeps_runtime_responsive --nocapture\n",
    )
    .unwrap();
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755)).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut child = tokio::process::Command::new(&exe)
        .args([
            "--exact",
            "local_thread_tests::git_workspace_lookup_keeps_runtime_responsive",
            "--nocapture",
        ])
        .env("MHC_GIT_TEST_MODE", "worker")
        .env("MHC_GIT_TEST_EXE", &exe)
        .env(
            "MHC_GIT_TEST_ADDRESS",
            listener.local_addr().unwrap().to_string(),
        )
        .env("PATH", tmp.path())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        let mut control = None;
        let mut git = None;
        for _ in 0..2 {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut kind = [0; 1];
            stream.read_exact(&mut kind).await.unwrap();
            match kind[0] {
                b'C' => control = Some(stream),
                b'G' => git = Some(stream),
                other => panic!("unexpected test connection: {other}"),
            }
        }
        let mut control = control.expect("runtime control connection");
        let mut git = git.expect("blocked git connection");
        control.write_all(b"S").await.unwrap();
        let mut response = [0; 1];
        control.read_exact(&mut response).await.unwrap();
        assert_eq!(
            &response, b"T",
            "runtime must progress while git is blocked"
        );
        git.write_all(b"R").await.unwrap();
        assert!(child.wait().await.unwrap().success());
    })
    .await
    .expect("workspace lookup blocked the runtime");
}

#[tokio::test]
async fn local_addressing_resolves_names_prefixes_and_refuses_remote_without_service() {
    assert_eq!(normalize_name("Alpha  Beta"), "alpha beta");
    assert_eq!(
        parse_address("dev-b/worker-1"),
        ParsedAddress::Device {
            text: "dev-b/worker-1".into(),
            device: "dev-b".into(),
            thread: "worker-1".into(),
        }
    );

    let s1 = ThreadSummary {
        thread_id: "11111111-1111-4111-8111-111111111111".into(),
        name: "Alpha Build".into(),
        alias: Some("abuild".into()),
        cwd: "/repo".into(),
        status: ThreadStatus::Live,
        created_at: "2026-11-24T00:00:00Z".into(),
        updated_at: None,
        error_note: None,
        endpoint: None,
        surface: None,
        alive: Some(true),
        controls: None,
        skills: None,
        device: None,
    };
    assert_eq!(
        row_name(&NamedRow {
            name: Some(&s1.name),
            first_message: None,
            folder: &s1.cwd,
        })
        .name,
        "Alpha Build"
    );

    let mut e1 = ThreadAddressEntry::from_summary(s1);
    e1.aliases = Some(vec!["abuild".into()]);
    let ctx = ResolveContext::local_only("caller-1", "/repo", vec![e1]);

    // Exact alias & unique UUID prefix resolve locally.
    let by_alias = resolve_address(&ctx, "abuild", &ResolveOptions::default())
        .await
        .expect("resolve alias");
    assert_eq!(by_alias.thread, "11111111-1111-4111-8111-111111111111");
    assert!(matches!(by_alias.where_located, ResolveWhere::Here { .. }));

    let by_prefix = resolve_address(&ctx, "11111111", &ResolveOptions::default())
        .await
        .expect("resolve prefix");
    assert_eq!(by_prefix.thread, "11111111-1111-4111-8111-111111111111");

    // Cross-device address in O01 returns typed remote_unavailable refusal.
    let remote_err = resolve_address(&ctx, "macbook/Alpha Build", &ResolveOptions::default())
        .await
        .expect_err("remote unavailable");
    assert_eq!(remote_err.code.as_str(), "remote_unavailable");

    // Addressing wrapper reports typed failure when names collide.
    let e2 = ThreadAddressEntry::from_summary(ThreadSummary {
        thread_id: "22222222-2222-4222-8222-222222222222".into(),
        name: "Alpha Build".into(),
        alias: None,
        cwd: "/repo".into(),
        status: ThreadStatus::Live,
        created_at: "2026-11-24T00:00:00Z".into(),
        updated_at: None,
        error_note: None,
        endpoint: None,
        surface: None,
        alive: Some(true),
        controls: None,
        skills: None,
        device: None,
    });
    let mut amb_ctx = ctx.clone();
    amb_ctx.local.push(e2);
    let amb_err = resolve_address(&amb_ctx, "Alpha Build", &ResolveOptions::default())
        .await
        .expect_err("ambiguous name");
    assert_eq!(amb_err.code.as_str(), "ambiguous_target");
}

// ---------------------------------------------------------------------------
// 4. Git worktree creation & discard
// ---------------------------------------------------------------------------

#[tokio::test]
async fn git_worktree_creates_detached_branch_and_discards_cleanly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("repo");
    let wt_root = tmp.path().join("worktrees");
    std::fs::create_dir_all(&repo).expect("mkdir repo");

    let run = |args: &[&str]| {
        let out = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .expect("git");
        assert!(
            out.status.success(),
            "git {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    };
    run(&["init"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    std::fs::write(repo.join("README.md"), "hello\n").expect("write");
    run(&["add", "README.md"]);
    run(&["commit", "-m", "init"]);

    let req = WorktreeRequest {
        branch: Some("feat-p10c".into()),
        base: Some("HEAD".into()),
    };
    let created = match create_thread_worktree(&repo, &req, &wt_root, None).await {
        WorktreeOutcome::Ok(ok) => ok,
        WorktreeOutcome::Error(err) => panic!("worktree failed: {err:?}"),
    };
    assert_eq!(created.branch, "feat-p10c");
    assert!(Path::new(&created.path).exists());

    let git = run_git();
    let discarded = discard_worktree(DiscardWorktreeOptions {
        path: Path::new(&created.path),
        branch: &created.branch,
        repository: Path::new(&created.repository),
        root: &wt_root,
        git: &git,
        created_folder: None,
        remove_unregistered_folder: false,
    })
    .await;
    assert!(discarded.is_none(), "discard failed: {discarded:?}");
    assert!(!Path::new(&created.path).exists());
}

// ---------------------------------------------------------------------------
// 5. Control: close_ladder, set_model_on rollback, wait_for_turn_end, create_launch, rename
// ---------------------------------------------------------------------------

struct MockControl {
    id: String,
    models: Vec<ModelChoice>,
    state: Mutex<SessionModelState>,
    fail_thinking: bool,
}

impl SessionControl for MockControl {
    fn thread_id(&self) -> &str {
        &self.id
    }
    fn title(&self) -> &str {
        &self.id
    }
    fn visible(&self) -> BoxFuture<Result<Vec<ThreadSummary>, ControlError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn get_messages(&self) -> BoxFuture<Result<Vec<ThreadTranscriptEntry>, ControlError>> {
        Box::pin(async { Ok(Vec::new()) })
    }
    fn get_available_models(
        &self,
        _refresh: bool,
    ) -> BoxFuture<Result<AvailableModels, ControlError>> {
        let models = self.models.clone();
        Box::pin(async move {
            Ok(AvailableModels {
                models,
                held: false,
            })
        })
    }
    fn get_model_state(&self) -> BoxFuture<Result<Option<SessionModelState>, ControlError>> {
        let st = self.state.lock().expect("lock").clone();
        Box::pin(async move { Ok(Some(st)) })
    }
    fn set_model(
        &self,
        provider: &str,
        model_id: &str,
    ) -> BoxFuture<Result<maho_omo_gateway::thread::contracts::state::ThreadModel, ControlError>>
    {
        let mut st = self.state.lock().expect("lock");
        st.model = maho_omo_gateway::thread::contracts::state::ThreadModel {
            provider: provider.to_owned(),
            id: model_id.to_owned(),
        };
        let m = st.model.clone();
        Box::pin(async move { Ok(m) })
    }
    fn set_thinking_level(&self, level: &str) -> BoxFuture<Result<(), ControlError>> {
        if self.fail_thinking && level == "high" {
            return Box::pin(async {
                Err(ControlError::Transport(
                    "thinking_level_unsupported:unsupported".to_owned(),
                ))
            });
        }
        self.state.lock().expect("lock").thinking_level = level.to_owned();
        Box::pin(async { Ok(()) })
    }
    fn get_available_thinking_levels(&self) -> BoxFuture<Result<Vec<String>, ControlError>> {
        Box::pin(async { Ok(vec!["off".into(), "low".into()]) })
    }
    fn set_name(&self, _name: &str) -> BoxFuture<Result<(), ControlError>> {
        Box::pin(async { Ok(()) })
    }
    fn interrupt(
        &self,
        _turn_id: Option<&str>,
    ) -> BoxFuture<
        Result<maho_omo_gateway::thread::control::interrupt::InterruptOutcome, ControlError>,
    > {
        Box::pin(async {
            Ok(
                maho_omo_gateway::thread::control::interrupt::InterruptOutcome {
                    interrupted: false,
                    turn_id: None,
                    background: ThreadInterruptBackground::NotSupported,
                },
            )
        })
    }
    fn close(
        &self,
        options: ThreadCloseOptions,
    ) -> BoxFuture<Result<ThreadCloseAnswer, ControlError>> {
        let outcome = if options.force == Some(true) {
            ThreadCloseOutcome::Closed
        } else {
            ThreadCloseOutcome::HeldElsewhere
        };
        Box::pin(async move {
            Ok(ThreadCloseAnswer {
                outcome,
                dropped_deliveries: Vec::new(),
                graceful: None,
            })
        })
    }
    fn wait_settled(&self, timeout_ms: u64) -> BoxFuture<Result<ThreadWaitOutcome, ControlError>> {
        Box::pin(async move {
            let _ = timeout_ms;
            Ok(ThreadWaitOutcome {
                outcome: "settled".into(),
                status: "idle".into(),
                waited_ms: 0,
                turn_id: Some("turn-99".into()),
            })
        })
    }
    fn running(&self) -> BoxFuture<Result<bool, ControlError>> {
        Box::pin(async { Ok(false) })
    }
    fn set_archived(&self, _archived: bool) -> BoxFuture<Result<(), ControlError>> {
        Box::pin(async { Ok(()) })
    }
    fn listed(&self, _archived: bool) -> BoxFuture<Result<Value, ControlError>> {
        Box::pin(async { Ok(json!({ "threads": [] })) })
    }
}

struct MockWatch(Mutex<Option<oneshot::Receiver<()>>>);
impl SessionWatch for MockWatch {
    fn initial(&self) -> Option<SessionWatchInitial> {
        None
    }
    fn ended(&self) -> BoxFuture<String> {
        let rx = self.0.lock().expect("lock").take();
        Box::pin(async move {
            if let Some(rx) = rx {
                let _ = rx.await;
            }
            "done".into()
        })
    }
    fn stop(&self) {}
}

#[tokio::test]
async fn control_close_ladder_model_rollback_wait_settled_launch_and_rename() {
    // close_ladder: refuse deliveries + unload_graceful fallback / held_elsewhere handling.
    let commands: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let cmd_log = commands.clone();
    let ans = close_ladder(
        "sess-1",
        Box::new(move |cmd: Map<String, Value>, _timeout| {
            cmd_log
                .lock()
                .expect("lock")
                .push(Value::Object(cmd.clone()));
            let typ = cmd
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            Box::pin(async move {
                let mut m = Map::new();
                match typ.as_str() {
                    "get_protocol_info" => {
                        m.insert(
                            "capabilities".into(),
                            json!(["unload_refuse_deliveries", "unload_graceful"]),
                        );
                        Ok(RecordAnswer::Ok(m))
                    }
                    "unload_session" => {
                        m.insert("outcome".into(), json!("unloaded"));
                        m.insert("released".into(), json!(true));
                        m.insert("unloaded".into(), json!(true));
                        m.insert("graceful".into(), json!({ "turn": 1, "waited_ms": 12 }));
                        Ok(RecordAnswer::Ok(m))
                    }
                    _ => Ok(RecordAnswer::Ok(m)),
                }
            })
        }),
        ThreadCloseOptions {
            force: None,
            graceful: Some(true),
            wait_ms: Some(5_000),
        },
        None,
    )
    .await
    .expect("close ladder");
    assert_eq!(ans.outcome, ThreadCloseOutcome::Closed);
    assert_eq!(ans.graceful.as_ref().map(|g| g.waited_ms), Some(12));

    // close_thread reports foreign_live_owner when HeldElsewhere.
    let ctrl_held = MockControl {
        id: "t-1".into(),
        models: vec![
            ModelChoice {
                provider: "anthropic".into(),
                id: "claude-sonnet-4".into(),
                name: Some("Claude Sonnet 4".into()),
            },
            ModelChoice {
                provider: "openai".into(),
                id: "gpt-5".into(),
                name: Some("GPT-5".into()),
            },
        ],
        state: Mutex::new(SessionModelState {
            model: maho_omo_gateway::thread::contracts::state::ThreadModel {
                provider: "anthropic".into(),
                id: "claude-sonnet-4".into(),
            },
            thinking_level: "low".into(),
        }),
        fail_thinking: true,
    };
    let held_resp = close_thread(&ctrl_held, ThreadCloseOptions::default())
        .await
        .expect("close_thread");
    assert_eq!(held_resp["kind"], "error");
    assert_eq!(held_resp["error"]["code"], "foreign_live_owner");

    // set_model_on with `:high` rolls back to previous model when thinking_level fails.
    let model_resp = set_model_on(&ctrl_held, "gpt-5:high", None)
        .await
        .expect("set_model_on");
    assert_eq!(model_resp["kind"], "error");
    assert_eq!(
        ctrl_held.state.lock().expect("lock").model.id,
        "claude-sonnet-4"
    );

    // wait_for_turn_end settles on agent_settled event without polling/sleep.
    let (tx, rx) = oneshot::channel();
    let (_end_tx, end_rx) = oneshot::channel::<()>();
    let setter = tokio::spawn(async move {
        let _ = tx.send(());
    });
    let wait_res = tokio::time::timeout(
        Duration::from_secs(2),
        wait_for_turn_end(
            move |handlers| {
                Box::pin(async move {
                    let _ = rx.await;
                    handlers.record(&json!({ "type": "agent_settled", "turnId": "turn-99" }));
                    Ok(SessionWatchOutcome::Ok(Arc::new(MockWatch(Mutex::new(
                        Some(end_rx),
                    )))))
                })
            },
            |reason| refuse("unsupported", reason, "Retry", None),
            2_000,
        ),
    )
    .await
    .expect("timeout")
    .expect("wait_for_turn_end");
    let _ = setter.await;
    assert_eq!(wait_res.outcome, "settled");
    assert_eq!(wait_res.turn_id.as_deref(), Some("turn-99"));

    // create_launch preset narrowing: parent has "edit" only, child asks "all" -> cannot widen.
    let create_input: ThreadCreateInput = serde_json::from_value(json!({
        "name": "child-1",
        "permission_preset": "all"
    }))
    .expect("create input");
    let plan = create_launch(&create_input, Some("edit"));
    assert!(matches!(
        plan,
        maho_omo_gateway::thread::control::create_launch::CreateLaunchPlan::Error(_)
    ));

    // rename_thread resolves local session via locate_control and sets its name/alias.
    struct RenameMockTransport {
        cwd: String,
        socket: String,
    }
    impl RpcTransport for RenameMockTransport {
        fn request(
            &self,
            _socket: &str,
            command: Value,
            _durable_session_id: Option<String>,
        ) -> BoxFuture<Result<Value, String>> {
            let cmd = command
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned();
            let cwd = self.cwd.clone();
            let socket = self.socket.clone();
            Box::pin(async move {
                let data = match cmd.as_str() {
                    "list_sessions" => json!({
                        "sessions": [{
                            "sessionId": "sess-a",
                            "durableSessionId": "dur-a",
                            "cwd": cwd,
                            "name": "Alpha",
                            "status": "idle",
                            "socket": socket,
                        }]
                    }),
                    "set_session_name" => json!({
                        "name": command.get("name").cloned().unwrap_or(json!("alpha-alias"))
                    }),
                    _ => json!({}),
                };
                Ok(json!({
                    "type": "response",
                    "command": cmd,
                    "success": true,
                    "data": data
                }))
            })
        }
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let store = test_store(tmp.path(), 1_000);
    let tmp_str = tmp.path().to_string_lossy().into_owned();
    let sock_str = tmp
        .path()
        .join("rename.sock")
        .to_string_lossy()
        .into_owned();
    let host: Arc<dyn ThreadHost> = Arc::new(LiveThreadSurface::new(
        maho_omo_gateway::thread::live_surface::LiveThreadSurfaceOptions {
            socket: sock_str.clone(),
            transport: Arc::new(RenameMockTransport {
                cwd: tmp_str.clone(),
                socket: sock_str.clone(),
            }),
            status_all: None,
            now: None,
        },
    ));
    let opts = ThreadToolSurfaceOptions {
        host,
        store: store.clone(),
        caller_session_id: Arc::new(|| "caller-1".into()),
        caller_workspace_root: Arc::new({
            let s = tmp_str.clone();
            move || s.clone()
        }),
        state_directory: tmp.path().to_path_buf(),
        disk_sessions: None,
        sessions_directory: None,
        ensure_host: None,
        same_device_wake: None,
        load_provenance_workspaces: None,
        on_provenance_warning: None,
        caller_turn_id: None,
        caller_name: None,
        caller_cause: None,
        caller_chain_hop: None,
        caller_run_deliveries: None,
        caller_reply_targets: None,
        caller_reply_senders: None,
        caller_run_has_local_input: None,
        on_completion_armed: None,
        permission_preset: None,
        now: Some(Arc::new(|| 1_000)),
        first_prompt_wait_ms: None,
        remote: None,
        worktree_root: None,
    };
    let view = ThreadHostView {
        sessions: vec![ThreadHostSession {
            session_id: "sess-a".into(),
            durable_session_id: Some("dur-a".into()),
            cwd: tmp_str.clone(),
            name: Some("Alpha".into()),
            status: Some("idle".into()),
            socket: Some(sock_str.clone()),
            ..Default::default()
        }],
        hosts: vec![maho_omo_gateway::thread::address_book::AddressBookHost {
            socket: sock_str,
            list_sessions: Some(
                maho_omo_gateway::thread::address_book::HostListSessions::ok(vec![
                    maho_omo_gateway::thread::address_book::HostSession {
                        session_id: "sess-a".into(),
                        durable_session_id: Some("dur-a".into()),
                        session_path: None,
                        cwd: tmp_str,
                        name: Some("Alpha".into()),
                        status: maho_omo_gateway::thread::address_book::HostSessionStatus::parse(
                            "idle",
                        ),
                        kind: None,
                        created_at: None,
                        updated_at: None,
                    },
                ]),
            ),
            error: None,
            endpoint_kind: GatewayEndpointKind::RpcHost,
            alive: Some(true),
            reason: None,
            legacy: true,
        }],
        disk: Vec::new(),
    };
    let rename_in = ThreadRenameInput {
        thread: Some("Alpha".into()),
        all_scope: None,
        name: None,
        alias: Some("alpha-alias".into()),
        idempotency_key: None,
    };
    let renamed = rename_thread(&opts, &view, &rename_in, "rk-1", "caller-1")
        .await
        .expect("rename");
    assert_eq!(renamed["kind"], "ok", "renamed={renamed}");
    assert_eq!(renamed["name"], "alpha-alias");
    assert_eq!(renamed["alias_scope"], "local");
}

struct NoopTransport;
impl RpcTransport for NoopTransport {
    fn request(
        &self,
        _socket: &str,
        _command: Value,
        _durable_session_id: Option<String>,
    ) -> BoxFuture<Result<Value, String>> {
        Box::pin(async { Err("host_unavailable".into()) })
    }
}

// ---------------------------------------------------------------------------
// 6. Wake train: session_target, engine_minimum, host_ensure, Carrier
// ---------------------------------------------------------------------------

struct MockCarrierConn {
    closed: Arc<Notify>,
}

impl MockCarrierConn {
    fn new() -> Self {
        Self {
            closed: Arc::new(Notify::new()),
        }
    }
}

impl CarrierHostConnection for MockCarrierConn {
    fn request(&self, frame: Map<String, Value>) -> BoxFuture<Result<Value, String>> {
        let cmd = frame
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        Box::pin(async move {
            let data = match cmd.as_str() {
                "open_session" => json!({
                    "sessionId": "rpc-woken-1",
                    "attached": false
                }),
                "get_state" => json!({
                    "isStreaming": false,
                    "isCompacting": false,
                    "pendingMessageCount": 0,
                    "loaded": true,
                    "ownerAttached": false,
                    "retained": true
                }),
                "wake" => json!({ "accepted": true }),
                "list_sessions" => json!({ "sessions": [] }),
                "release_session" => json!({ "released": true }),
                _ => json!({}),
            };
            Ok(json!({
                "type": "response",
                "command": cmd,
                "success": true,
                "data": data
            }))
        })
    }

    fn subscribe_events(&self) -> mpsc::UnboundedReceiver<Value> {
        let (_tx, rx) = mpsc::unbounded_channel();
        rx
    }

    fn closed_notify(&self) -> Arc<Notify> {
        self.closed.clone()
    }

    fn is_closed(&self) -> bool {
        false
    }

    fn close(&self) {}
}

#[tokio::test]
async fn wake_session_target_engine_minimum_host_ensure_and_carrier() {
    assert!(engine_drops_retained_record(
        Some("2026.11.19-5"),
        MIN_WAKE_ENGINE_VERSION
    ));
    assert!(!engine_drops_retained_record(
        Some("2026.11.10-1"),
        MIN_WAKE_ENGINE_VERSION
    ));
    assert!(engine_drops_retained_record(
        Some("5.1.72"),
        MIN_WAKE_ENGINE_VERSION
    ));

    let ensured = parse_host_ensure_json(
        "noise\n{\"ok\":true,\"action\":\"started\",\"socket\":\"/run/h.sock\"}\n",
    )
    .expect("parse host ensure");
    assert!(ensured.ok);
    assert_eq!(ensured.socket.as_deref(), Some("/run/h.sock"));

    let status = parse_host_status_all(
        "{\"endpoints\":[{\"socket\":\"/run/h.sock\",\"reachable\":true,\"session_rows\":[]}]}",
    )
    .expect("parse host status");
    assert_eq!(status.len(), 1);

    let tmp = tempfile::tempdir().expect("tempdir");
    let sess_dir = tmp.path().join("sessions");
    let proj_dir = sess_dir.join("--repo--");
    std::fs::create_dir_all(&proj_dir).expect("mkdir");
    let spath = proj_dir.join("2026-11-24_dur-wake-1.jsonl");
    std::fs::write(
        &spath,
        "{\"type\":\"session\",\"id\":\"dur-wake-1\",\"cwd\":\"/repo\",\"timestamp\":\"2026-11-24T00:00:00Z\"}\n",
    )
    .expect("write session file");

    let target = resolve_wake_target(&sess_dir, "dur-wake-1");
    assert!(matches!(
        target,
        WakeTarget::Found {
            session_kind: WakeSessionKind::Interactive,
            closed_at: None,
            ..
        }
    ));

    let store = test_store(tmp.path(), 5_000);
    let _ = store.enqueue(&EnqueueRequest {
        provenance: Provenance {
            workspace: "ws".into(),
            project: "proj".into(),
            lane: None,
            session: None,
            device: None,
            device_absent: None,
        },
        now: 5_000,
        delivery_id: "wake-del-1".into(),
        target_durable_id: "dur-wake-1".into(),
        sender_principal: "session:caller-1".into(),
        rate_principal: None,
        sender_node: "session:caller-1".into(),
        sender_turn: None,
        cause_delivery_id: None,
        inbound_hop: None,
        authority: None,
        claimed_root_id: None,
        origin: EnvelopeOrigin::Session {
            session: "caller-1".into(),
        },
        actor: "caller-1".into(),
        body: "wake up".into(),
        mode: GatewayDeliveryMode::Auto,
        expected_turn_id: None,
        binding: None,
        receipt: ReceiptRef {
            idempotency_key: "wake-k1".into(),
            args_hash: "wake-h1".into(),
        },
        endpoint_kind: None,
        collapse_key: None,
    });

    let carrier = Carrier::new(CarrierOptions {
        store: store.clone(),
        sessions_directory: sess_dir,
        ensure_host: Arc::new(|| Box::pin(async {})),
        connect: Arc::new(|| {
            Box::pin(async {
                Ok(Arc::new(MockCarrierConn::new()) as Arc<dyn CarrierHostConnection>)
            })
        }),
        wake_limit: None,
        same_device: Some(Arc::new(|_| true)),
        remote_wake: None,
        admit_intake: None,
        memory_headroom: None,
        memory_floor: None,
        on_event: None,
    });

    let _row = WokenSessionRow {
        durable_id: "dur-wake-1".into(),
        session_file: spath.to_string_lossy().into_owned(),
        host_instance: None,
        state: WokenSessionState::Requested,
        requested_at: 5_000,
        opened_at: None,
        first_waiting_at: None,
        lease_owner: None,
        lease_expires_at: None,
        generation: 1,
        attempts: 0,
        next_attempt_at: None,
    };
    let outcome = carrier.wake("dur-wake-1").await;
    assert!(
        matches!(outcome, CarrierWakeOutcome::Open { .. }),
        "outcome={outcome:?}"
    );
    carrier.stop();
}

// ---------------------------------------------------------------------------
// 7. Paged reader + model_match + reply_permission + thread_state
// ---------------------------------------------------------------------------

#[test]
fn paged_reader_and_tool_helpers_work() {
    let tail = ContextTail {
        revision: "rev-1".into(),
        messages: (1..=5)
            .map(|i| {
                let mut entry = ThreadTranscriptEntry::new();
                entry.insert("role".into(), json!("assistant"));
                entry.insert("text".into(), json!(format!("msg {i}")));
                TailMessage {
                    id: format!("m-{i}"),
                    message: entry,
                }
            })
            .collect(),
    };

    let page1 = select_entry_page(&tail, Some(40), None);
    let cursor = match page1 {
        EntryPageRead::Ok {
            next_cursor: Some(c),
            ..
        } => c,
        other => panic!("expected cursor on page 1, got {other:?}"),
    };
    assert!(is_entry_cursor(&cursor));

    let stale = select_entry_page(
        &ContextTail {
            revision: "rev-2".into(),
            messages: tail.messages.clone(),
        },
        None,
        Some(&cursor),
    );
    assert!(matches!(stale, EntryPageRead::Error { .. }));

    // model_match parses thinking suffix.
    let catalog = vec![ModelChoice {
        provider: "openai".into(),
        id: "gpt-5.4".into(),
        name: Some("GPT 5.4".into()),
    }];
    match match_model(&catalog, "gpt-5.4:xhigh", None) {
        ModelMatchResult::Match {
            model,
            thinking_level,
        } => {
            assert_eq!(model.id, "gpt-5.4");
            assert_eq!(thinking_level.as_deref(), Some("xhigh"));
        }
        other => panic!("unexpected match: {other:?}"),
    }

    // reply_permission allows reply to consumed sender by name/uuid.
    let perms = thread_send_permission(
        &json!({ "thread": "Reviewer" }),
        &["aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into()],
        &[ReplySender {
            session_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".into(),
            name: Some("Reviewer".into()),
            device: None,
        }],
        None,
    );
    assert_eq!(perms.len(), 1);
    assert_eq!(perms[0].permission, "thread_send");

    // thread_state rules (#13403).
    assert_eq!(
        format!(
            "{:?}",
            thread_state(None, true, Some(ThreadActivity::Running))
        ),
        "Running"
    );
    assert_eq!(
        format!("{:?}", thread_state(Some(true), true, None)),
        "Archived"
    );
    assert_eq!(format!("{:?}", thread_state(None, false, None)), "Inactive");
}

// ---------------------------------------------------------------------------
// 8. NativeThreadComponent (ExtensionSide::Brain vs Hand) + live Unix JSONL socket tools
// ---------------------------------------------------------------------------

#[test]
fn native_thread_component_registers_on_brain_only_with_eval_exposure() {
    let mut brain_api = ExtensionApi::new(
        LoadedExtension::new(
            "native-thread",
            PathBuf::from("/repo"),
            SourceInfo::default(),
        ),
        ExtensionSessionProfile {
            side: Some(ExtensionSide::Brain),
            ..Default::default()
        },
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    NativeThreadComponent.register(&mut brain_api);

    let names: Vec<&str> = brain_api
        .registered
        .tools
        .iter()
        .map(|t| t.definition.name.as_str())
        .collect();
    assert_eq!(names.len(), 17);
    assert!(names.contains(&"thread_create"));
    assert!(names.contains(&"thread_send"));
    assert!(names.contains(&"thread_read"));
    assert!(names.contains(&"thread_list"));
    assert!(names.contains(&"thread_close"));
    assert!(names.contains(&"thread_bind"));
    assert!(names.contains(&"thread_bindings"));

    for tool in &brain_api.registered.tools {
        assert_eq!(tool.definition.exposure, Some(ToolExposure::Eval));
        if tool.definition.name == "thread_create" {
            assert_eq!(
                tool.definition.prompt_guidelines.as_deref(),
                Some(&[THREAD_FAMILY_PROMPT_GUIDELINES.to_owned()][..])
            );
        }
    }

    let mut hand_api = ExtensionApi::new(
        LoadedExtension::new(
            "native-thread",
            PathBuf::from("/repo"),
            SourceInfo::default(),
        ),
        ExtensionSessionProfile {
            side: Some(ExtensionSide::Hand),
            ..Default::default()
        },
        EventBus::default(),
        ExtensionRuntime::default(),
    );
    NativeThreadComponent.register(&mut hand_api);
    assert!(
        hand_api.registered.tools.is_empty(),
        "Brain-only thread tools must NOT register on ExtensionSide::Hand"
    );
}

#[tokio::test]
async fn live_unix_socket_drives_thread_create_list_send_read_rename_set_model_and_close() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let store = test_store(tmp.path(), 50_000);
    let sock_path = tmp.path().join("host.sock");
    let listener = UnixListener::bind(&sock_path).expect("bind unix socket");

    // In-memory mock host state shared across accepted connections.
    struct HostMem {
        sessions: Vec<Value>,
        messages: HashMap<String, Vec<Value>>,
        model: Value,
        thinking: String,
        store: Arc<dyn GatewayStoreApi>,
    }
    let mem = Arc::new(Mutex::new(HostMem {
        sessions: vec![json!({
            "sessionId": "rpc-caller",
            "durableSessionId": "dur-caller",
            "cwd": tmp.path().to_string_lossy().to_string(),
            "name": "Caller",
            "status": "idle",
            "kind": "interactive"
        })],
        messages: HashMap::new(),
        model: json!({ "provider": "anthropic", "id": "claude-sonnet-4" }),
        thinking: "low".into(),
        store: store.clone(),
    }));

    let (stop_tx, mut stop_rx) = oneshot::channel::<()>();
    let mem_srv = mem.clone();
    let workspace_str = tmp.path().to_string_lossy().to_string();
    let srv_handle = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stop_rx => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break };
                    let mem_conn = mem_srv.clone();
                    let ws = workspace_str.clone();
                    tokio::spawn(async move {
                        let mut reader = BufReader::new(stream);
                        let mut line = String::new();
                        while reader.read_line(&mut line).await.unwrap_or(0) > 0 {
                            let req: Value = serde_json::from_str(line.trim()).unwrap_or(Value::Null);
                            line.clear();
                            let id = req.get("id").cloned().unwrap_or(Value::Null);
                            let cmd = req.get("type").and_then(Value::as_str).unwrap_or("");
                            let data = {
                                let mut st = mem_conn.lock().expect("lock");
                                match cmd {
                                    "get_protocol_info" => json!({
                                        "capabilities": [
                                            "attach_session",
                                            "release_attachment",
                                            "release_caller_aware",
                                            "unload_graceful",
                                            "unload_refuse_deliveries"
                                        ]
                                    }),
                                    "attach_session" => json!({
                                        "attached": true,
                                        "durableSessionId": req.get("sessionId").cloned().unwrap_or(json!("dur-1")),
                                        "holder": "rpc_host"
                                    }),
                                    "release_attachment" => json!({ "released": true }),
                                    "list_sessions" => json!({ "sessions": st.sessions.clone() }),
                                    "open_session" => {
                                        let name = req.get("name").and_then(Value::as_str).unwrap_or("Child");
                                        let idx = st.sessions.len();
                                        let sid = format!("rpc-child-{idx}");
                                        let fallback_did = format!("dur-child-{idx}");
                                        let durable = req
                                            .get("durableSessionId")
                                            .and_then(Value::as_str)
                                            .unwrap_or(&fallback_did);
                                        let row = json!({
                                            "sessionId": sid,
                                            "durableSessionId": durable,
                                            "cwd": ws,
                                            "name": name,
                                            "status": "open",
                                            "kind": "interactive"
                                        });
                                        st.sessions.push(row);
                                        st.messages.insert(sid.clone(), vec![json!({
                                            "role": "assistant",
                                            "text": "ready"
                                        })]);
                                        json!({
                                            "sessionId": sid,
                                            "state": { "sessionId": durable, "isStreaming": false }
                                        })
                                    }
                                    "set_session_name" => {
                                        let sid = req.get("sessionId").and_then(Value::as_str).unwrap_or("");
                                        let name = req.get("name").and_then(Value::as_str).unwrap_or("");
                                        for s in &mut st.sessions {
                                            if s.get("sessionId").and_then(Value::as_str) == Some(sid) {
                                                s["name"] = Value::String(name.to_owned());
                                            }
                                        }
                                        json!({ "name": name })
                                    }
                                    "get_state" => json!({
                                        "isStreaming": false,
                                        "model": st.model.clone(),
                                        "thinkingLevel": st.thinking.clone()
                                    }),
                                    "get_messages" => {
                                        let sid = req.get("sessionId").and_then(Value::as_str).unwrap_or("");
                                        json!({ "messages": st.messages.get(sid).cloned().unwrap_or_default() })
                                    }
                                    "get_available_models" => json!({
                                        "models": [
                                            { "provider": "anthropic", "id": "claude-sonnet-4", "name": "Sonnet 4" },
                                            { "provider": "openai", "id": "gpt-5.4", "name": "GPT 5.4" }
                                        ]
                                    }),
                                    "get_available_thinking_levels" => json!({
                                        "levels": ["off", "low", "medium", "high"]
                                    }),
                                    "set_model" => {
                                        let p = req.get("provider").and_then(Value::as_str).unwrap_or("");
                                        let m = req.get("modelId").and_then(Value::as_str).unwrap_or("");
                                        st.model = json!({ "provider": p, "id": m });
                                        st.model.clone()
                                    }
                                    "set_thinking_level" => {
                                        st.thinking = req.get("level").and_then(Value::as_str).unwrap_or("low").to_owned();
                                        json!({ "level": st.thinking.clone() })
                                    }
                                    "wake" => {
                                        let ids = req
                                            .get("delivery_ids")
                                            .and_then(Value::as_array)
                                            .cloned()
                                            .unwrap_or_default();
                                        let self_id = maho_omo_gateway::thread::types::ProcessIdentity {
                                            pid: std::process::id() as i64,
                                            process_start_time: None,
                                            instance_id: "mock-host".into(),
                                            runtime_instance: Some("mock-host".into()),
                                        };
                                        let mut admitted = Vec::new();
                                        for id in ids.iter().filter_map(|v| v.as_str()) {
                                            let _ = st.store.claim(&maho_omo_gateway::thread::types::ClaimRequest {
                                                now: 50_000,
                                                delivery_id: id.to_owned(),
                                                self_identity: self_id.clone(),
                                                lane: maho_omo_gateway::thread::types::EffectiveLane::Start,
                                            });
                                            let _ = st.store.record_outcome(&maho_omo_gateway::thread::types::RecordOutcomeRequest {
                                                now: 50_000,
                                                delivery_id: id.to_owned(),
                                                self_identity: self_id.clone(),
                                                outcome: maho_omo_gateway::thread::types::OutcomeRecord::Admitted {
                                                    admission_kind: "started".into(),
                                                    turn_epoch: 1,
                                                    wait_kind: None,
                                                },
                                            });
                                            admitted.push(json!({ "delivery_id": id, "kind": "started" }));
                                        }
                                        json!({ "admitted": admitted })
                                    }
                                    "unload_session" | "release_session" => {
                                        let sid = req.get("sessionId").and_then(Value::as_str).unwrap_or("");
                                        st.sessions.retain(|s| s.get("sessionId").and_then(Value::as_str) != Some(sid));
                                        json!({ "outcome": "unloaded", "released": true, "unloaded": true })
                                    }
                                    _ => json!({}),
                                }
                            };
                            let resp = json!({
                                "id": id,
                                "type": "response",
                                "command": cmd,
                                "success": true,
                                "data": data
                            });
                            let mut out = serde_json::to_vec(&resp).expect("ser");
                            out.push(b'\n');
                            if reader.get_mut().write_all(&out).await.is_err() {
                                break;
                            }
                        }
                    });
                }
            }
        }
    });

    let sock_str = sock_path.to_string_lossy().into_owned();
    let sock_ep = sock_str.clone();
    let surface_impl = Arc::new(LiveThreadSurface::new(LiveThreadSurfaceOptions {
        socket: sock_str,
        transport: Arc::new(UnixJsonlTransport {
            agent_dir: tmp.path().to_path_buf(),
            timeout_ms: 5_000,
        }),
        status_all: Some(Arc::new(move || {
            Some(vec![HostEndpointReport {
                socket: Some(sock_ep.clone()),
                reachable: true,
                session_paths: Vec::new(),
                endpoint_kind: Some(GatewayEndpointKind::RpcHost),
                alive: Some(true),
                reason: None,
            }])
        })),
        now: Some(Arc::new(|| 50_000)),
    }));
    let built = build_local_thread_tools(Arc::new(ThreadToolSurfaceOptions {
        host: surface_impl,
        store,
        caller_session_id: Arc::new(|| "dur-caller".into()),
        caller_workspace_root: Arc::new({
            let p = tmp.path().to_string_lossy().into_owned();
            move || p.clone()
        }),
        state_directory: tmp.path().to_path_buf(),
        disk_sessions: None,
        sessions_directory: None,
        ensure_host: None,
        same_device_wake: None,
        load_provenance_workspaces: None,
        on_provenance_warning: None,
        caller_turn_id: None,
        caller_name: Some(Arc::new(|| Some("Caller".into()))),
        caller_cause: None,
        caller_chain_hop: None,
        caller_run_deliveries: None,
        caller_reply_targets: None,
        caller_reply_senders: None,
        caller_run_has_local_input: None,
        on_completion_armed: None,
        permission_preset: None,
        now: Some(Arc::new(|| 50_000)),
        first_prompt_wait_ms: None,
        remote: None,
        worktree_root: None,
    }));

    let call = |name: &str, call_id: &str, args: Value| {
        let tool = built
            .tools
            .iter()
            .find(|t| t.name == name)
            .expect("tool present");
        (tool.execute)(call_id, args)
    };

    // 1. thread_create
    let created = call("thread_create", "c-1", json!({ "name": "worker-alpha" })).await;
    assert_eq!(created["kind"], "ok", "created={created}");
    let child_id = created["thread"].as_str().expect("thread id").to_owned();
    assert_eq!(created["name"], "worker-alpha", "created={created}");

    // 2. thread_list
    let listed = call("thread_list", "c-2", json!({})).await;
    assert_eq!(listed["kind"], "ok", "listed={listed}");

    // 3. thread_send (must unwrap wake reply data.admitted & transition delivery row to started)
    let sent = call(
        "thread_send",
        "c-3",
        json!({ "thread": "worker-alpha", "message": "ping from test" }),
    )
    .await;
    assert_eq!(sent["kind"], "ok", "sent={sent}");
    assert_eq!(sent["delivery"]["kind"], "started", "sent={sent}");

    // 3b. thread_create with prompt verifies end-to-end open + rename + wake unwrapping
    let created_with_prompt = call(
        "thread_create",
        "c-3b",
        json!({ "name": "worker-beta", "prompt": "boot prompt" }),
    )
    .await;
    assert_eq!(
        created_with_prompt["kind"], "ok",
        "created_with_prompt={created_with_prompt}"
    );
    assert_eq!(
        created_with_prompt["name"], "worker-beta",
        "created_with_prompt={created_with_prompt}"
    );
    assert_eq!(
        created_with_prompt["prompt"]["delivery"]["kind"], "started",
        "created_with_prompt={created_with_prompt}"
    );
    let _ = child_id;

    // 4. thread_read
    let read = call("thread_read", "c-4", json!({ "thread": "worker-alpha" })).await;
    assert_eq!(read["kind"], "ok", "read={read}");

    // 5. thread_rename
    let renamed = call(
        "thread_rename",
        "c-5",
        json!({ "thread": "worker-alpha", "name": "worker-renamed" }),
    )
    .await;
    assert_eq!(renamed["kind"], "ok", "renamed={renamed}");

    // 6. thread_set_model (with :high suffix)
    let switched = call(
        "thread_set_model",
        "c-6",
        json!({ "thread": "worker-renamed", "model": "gpt-5.4:high" }),
    )
    .await;
    assert_eq!(switched["kind"], "ok", "switched={switched}");

    // 7. thread_close
    let closed = call("thread_close", "c-7", json!({ "thread": "worker-renamed" })).await;
    assert_eq!(closed["kind"], "ok", "closed={closed}");

    (built.dispose)();
    let _ = stop_tx.send(());
    let _ = srv_handle.await;

    // Also exercise fenced_command directly against live mock endpoint.
    assert!(matches!(
        fenced_command(
            &NoopTransport,
            "/tmp/none.sock",
            json!({}),
            &Default::default(),
            None,
            &|| Box::pin(async { Ok(json!({ "ok": true })) })
        )
        .await,
        Ok(FenceOutcome::Ran(_))
    ));
}
