//! senpi `micro-session-lock-loss.test.ts` and `micro-runtime-lock-loss.test.ts` (target 59d58cee).
//!
//! Another process "reclaims" the session lock by replacing the lock's owner token, which is what a
//! stale-lock takeover does on disk; the next ownership check reports the loss to `on_lost`.

use std::path::Path;
use std::rc::Rc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::pico3::jsonl::JsonlStorage;
use maho_agent::harness::pico3::types::{DocRef, Storage};
use maho_ai::model::Model;
use maho_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxModelDefinition, FauxProviderState, FauxResponseStep,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider,
};
use maho_ai::types::{
    AssistantMessage, BoxFuture, ContentBlock, Context, SimpleStreamOptions, StopReason,
};
use maho_ai::utils::abort::AbortSignal;
use maho_cli::experimental::micro::api::{MicroController, MicroView, MicroViewSource};
use maho_cli::experimental::micro::lock_gated_storage::lock_gated_storage;
use maho_cli::experimental::micro::runtime::{
    LOST_LOCK_MESSAGE, OpenMicroDeps, OpenMicroOptions, OpenMicroResult, open_micro_with,
};
use maho_cli::experimental::micro::sessions::select_session_in;
use maho_core::file_lock::{FileLockLostError, OnLostCallback};
use maho_core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};

fn session_bytes(path: &Path) -> u64 {
    std::fs::read_dir(path)
        .expect("session dir")
        .map(|entry| entry.expect("entry").metadata().expect("metadata").len())
        .sum()
}

/// Another process takes the lock over: the owner token no longer matches ours.
fn reclaim(session: &Path) {
    let mut name = session.file_name().expect("name").to_os_string();
    name.push(".lock");
    std::fs::write(
        session.with_file_name(name).join("owner-token"),
        "another-process",
    )
    .expect("replace owner token");
}

#[tokio::test]
async fn once_another_process_reclaims_the_session_lock_the_session_gets_no_further_writes() {
    let agent = tempfile::tempdir().expect("agent dir");
    let cwd = tempfile::tempdir().expect("cwd");
    let lost = Arc::new(AtomicUsize::new(0));
    let on_lost: OnLostCallback = {
        let lost = lost.clone();
        Some(Arc::new(move |_: &FileLockLostError| {
            lost.fetch_add(1, Ordering::SeqCst);
        }))
    };
    let mut location =
        select_session_in(agent.path(), cwd.path(), false, on_lost).expect("session");
    let storage = Rc::new(JsonlStorage::open(&location.path, true).expect("storage"));
    let held = location.held_lock();
    let gated = lock_gated_storage(
        storage.clone(),
        Rc::new(move || held.as_ref().map_or(Ok(()), |held| held.assert_held())),
    );
    let ctx = &*BACKGROUND_CONTEXT;

    gated.commit(&[], ctx).await.expect("held commit");
    let written = session_bytes(&location.path);
    assert!(written > 0);

    reclaim(&location.path);
    assert_eq!(
        location.assert_held().expect_err("lost").code(),
        "ELOCKLOST"
    );
    assert_eq!(lost.load(Ordering::SeqCst), 1);

    let commit = gated.commit(&[], ctx).await.expect_err("refused commit");
    assert!(commit.to_string().contains("ELOCKLOST"), "{commit}");
    let truncate = gated
        .truncate(DocRef::Sticky { conversation_id: 1 }, ctx)
        .await
        .expect_err("refused truncate");
    assert!(truncate.to_string().contains("ELOCKLOST"), "{truncate}");
    assert_eq!(session_bytes(&location.path), written);
    assert_eq!(
        serde_json::to_string(&gated.conversations(ctx).await.expect("gated")).expect("json"),
        serde_json::to_string(&storage.conversations(ctx).await.expect("raw")).expect("json"),
    );
    assert_eq!(lost.load(Ordering::SeqCst), 1);
    storage.close(ctx).await.expect("close");
    location.release();
}

async fn within<T>(what: &str, future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {what}"))
}

/// Resolves once the view satisfies `predicate`; subscribes before checking.
async fn view_reaches(micro: &OpenMicroResult, predicate: fn(&MicroView) -> bool) -> MicroView {
    let notify = Rc::new(tokio::sync::Notify::new());
    let unsubscribe = {
        let notify = notify.clone();
        micro.view.subscribe(Box::new(move || notify.notify_one()))
    };
    loop {
        let view = micro.view.current();
        if predicate(&view) {
            unsubscribe();
            return view;
        }
        notify.notified().await;
    }
}

#[tokio::test]
async fn a_lost_session_lock_aborts_the_running_turn_and_no_further_session_write_lands() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let agent = tempfile::tempdir().expect("agent dir");
            let cwd = tempfile::tempdir().expect("cwd");
            // chatgpt-subscription impersonates the Codex client, so mhc refuses it until the user
            // has recorded consent (docs/divergences.md); this fixture stands for a consenting user.
            std::fs::write(
                agent.path().join("settings.json"),
                r#"{"providerRiskConsent":{"chatgpt-subscription":"2026-01-01T00:00:00.000Z"}}"#,
            )
            .expect("consent settings");
            // The micro default model, served by a faux provider whose turn stays in flight until
            // it is aborted.
            let faux = faux_provider(RegisterFauxProviderOptions {
                provider: Some("chatgpt-subscription".into()),
                models: Some(vec![FauxModelDefinition {
                    id: "gpt-5.6-sol".into(),
                    ..Default::default()
                }]),
                ..Default::default()
            })
            .expect("faux provider");
            let started: Arc<Mutex<Option<tokio::sync::oneshot::Sender<AbortSignal>>>> =
                Arc::new(Mutex::new(None));
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            *started.lock().expect("started") = Some(started_tx);
            faux.core
                .set_responses(vec![FauxResponseStep::Factory(Arc::new(
                    move |_context: &Context,
                          options: Option<&SimpleStreamOptions>,
                          _state: &FauxProviderState,
                          _model: &Model|
                          -> BoxFuture<'static, AssistantMessage> {
                        let signal = options
                            .and_then(|options| options.stream.request.signal.clone())
                            .expect("the micro turn must carry an abort signal");
                        if let Some(started) = started.lock().expect("started").take() {
                            let _ = started.send(signal.clone());
                        }
                        Box::pin(async move {
                            signal.cancelled().await;
                            faux_assistant_message(
                                Vec::<ContentBlock>::new(),
                                FauxAssistantMessageOptions {
                                    stop_reason: Some(StopReason::Aborted),
                                    ..Default::default()
                                },
                            )
                        })
                    },
                ))]);
            let mut model_runtime = ModelRuntime::create_sync(CreateModelRuntimeOptions {
                providers: Some(Vec::new()),
                auth_path: Some(agent.path().join("auth.json")),
                agent_dir: Some(agent.path().to_path_buf()),
                ..Default::default()
            })
            .expect("model runtime");
            model_runtime.register_native_provider(faux.provider.clone());

            let micro = open_micro_with(
                OpenMicroOptions {
                    cwd: Some(cwd.path().to_path_buf()),
                    continue_session: false,
                },
                OpenMicroDeps {
                    agent_dir: agent.path().to_path_buf(),
                    model_runtime,
                    on_harness: None,
                    debug_log: None,
                },
            )
            .await
            .expect("open micro");
            let session_path = std::path::PathBuf::from(micro.view.current().session.path);

            let controller = micro.controller.clone();
            tokio::task::spawn_local(async move { controller.prompt("hello".into()).await });
            let turn_signal = within("the faux turn to start", started_rx)
                .await
                .expect("turn started");
            let bytes_at_loss = session_bytes(&session_path);

            reclaim(&session_path);
            let lock = micro.view.session_lock().expect("host lock");
            assert!(lock.assert_held().is_err());

            let stopped = within(
                "micro to mark the session fatal",
                view_reaches(&micro, |view| view.fatal.is_some()),
            )
            .await;
            assert_eq!(stopped.fatal.as_deref(), Some(LOST_LOCK_MESSAGE));
            within("the running turn to be aborted", turn_signal.cancelled()).await;
            micro.controller.prompt("after the loss".into()).await;
            // The user quits: close joins every invocation and writes nothing.
            within("micro to close", micro.close()).await;
            assert_eq!(session_bytes(&session_path), bytes_at_loss);
        })
        .await;
}
