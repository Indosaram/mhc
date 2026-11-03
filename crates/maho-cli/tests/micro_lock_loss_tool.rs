//! senpi `micro-runtime-lock-loss-tool.test.ts` (target 59d58cee): the session lock is lost while a
//! tool call waits in a `beforeTool` hook (as a permission prompt does), so the stop path has a real
//! write to attempt (clearing the slot's `waitingOn`). The gated storage refuses it, the refusal is
//! logged by name only, and the stop still closes the views and the session files without
//! `micro.close()`.

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

use maho_agent::harness::context::BACKGROUND_CONTEXT;
use maho_agent::harness::pico3::harness::Harness;
use maho_agent::harness::pico3::kinds::tool::{
    BeforeTool, BeforeToolResult, StoredToolCall, ToolHooks,
};
use maho_agent::harness::pico3::types::{
    BeforeToolApi, Context, Error, LocalBoxFuture, NamespaceDefaults, Result,
};
use maho_ai::providers::faux::{
    FauxAssistantMessageOptions, FauxModelDefinition, FauxResponseStep,
    RegisterFauxProviderOptions, faux_assistant_message, faux_provider, faux_tool_call,
};
use maho_ai::types::StopReason;
use maho_cli::experimental::micro::api::{MicroController, MicroView, MicroViewSource};
use maho_cli::experimental::micro::runtime::{
    DebugLog, OpenMicroDeps, OpenMicroOptions, OpenMicroResult, open_micro_with,
};
use maho_core::model_runtime::{CreateModelRuntimeOptions, ModelRuntime};
use tokio::sync::oneshot;

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

/// Pins the higher-ranked `BeforeTool` signature on a closure that borrows its arguments.
fn before_tool<F>(hook: F) -> F
where
    F: for<'a> Fn(
        StoredToolCall,
        &'a dyn BeforeToolApi,
        &'a Context,
    ) -> LocalBoxFuture<'a, Result<Option<BeforeToolResult>>>,
{
    hook
}

/// A `beforeTool` hook that marks the slot waiting, reports it, then waits for the abort.
fn waiting_hook(waiting: Rc<RefCell<Option<oneshot::Sender<()>>>>) -> BeforeTool {
    Rc::new(before_tool(move |_call, api, ctx| {
        let waiting = waiting.clone();
        Box::pin(async move {
            api.waiting(ctx).await?;
            if let Some(waiting) = waiting.borrow_mut().take() {
                let _ = waiting.send(());
            }
            if let Some(signal) = ctx.abort_signal() {
                signal.cancelled().await;
            }
            Ok(None)
        })
    }))
}

#[tokio::test]
async fn closes_the_session_files_without_writing_when_the_lock_is_lost_while_a_tool_waits() {
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
            // The micro default model, answering with one read call.
            let faux = faux_provider(RegisterFauxProviderOptions {
                provider: Some("chatgpt-subscription".into()),
                models: Some(vec![FauxModelDefinition {
                    id: "gpt-5.6-sol".into(),
                    ..Default::default()
                }]),
                ..Default::default()
            })
            .expect("faux provider");
            let mut arguments = serde_json::Map::new();
            arguments.insert("path".into(), "missing.txt".into());
            faux.core
                .set_responses(vec![FauxResponseStep::from(faux_assistant_message(
                    vec![faux_tool_call("read", arguments, None)],
                    FauxAssistantMessageOptions {
                        stop_reason: Some(StopReason::ToolUse),
                        ..Default::default()
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

            let (waiting_tx, waiting_rx) = oneshot::channel();
            let waiting = Rc::new(RefCell::new(Some(waiting_tx)));
            let opened: Rc<RefCell<Option<Rc<Harness>>>> = Rc::default();
            let on_harness = {
                let opened = opened.clone();
                Box::new(move |harness: &Rc<Harness>| {
                    let namespace = harness
                        .namespace("test.lock-loss-wait", NamespaceDefaults::default(), None)
                        .expect("namespace");
                    let kinds = harness.kinds().expect("kinds");
                    // Kept for the test's lifetime: the unregister handle is dropped, not called.
                    let _unregister = harness
                        .hooks(
                            &namespace,
                            &kinds.tool,
                            Rc::new(ToolHooks {
                                before_tool: Some(waiting_hook(waiting)),
                                after_tool: None,
                            }),
                            None,
                            false,
                        )
                        .expect("tool hooks");
                    *opened.borrow_mut() = Some(harness.clone());
                })
            };
            let logged: Rc<RefCell<Vec<(String, String)>>> = Rc::default();
            let (stop_logged_tx, stop_logged_rx) = oneshot::channel();
            let debug_log: DebugLog = {
                let logged = logged.clone();
                let stop_logged = RefCell::new(Some(stop_logged_tx));
                Rc::new(move |header: &str, detail: &str| {
                    logged
                        .borrow_mut()
                        .push((header.to_owned(), detail.to_owned()));
                    if header.starts_with("micro session stop")
                        && let Some(signal) = stop_logged.borrow_mut().take()
                    {
                        let _ = signal.send(());
                    }
                })
            };

            let micro = open_micro_with(
                OpenMicroOptions {
                    cwd: Some(cwd.path().to_path_buf()),
                    continue_session: false,
                },
                OpenMicroDeps {
                    agent_dir: agent.path().to_path_buf(),
                    model_runtime,
                    on_harness: Some(on_harness),
                    debug_log: Some(debug_log),
                },
            )
            .await
            .expect("open micro");
            let harness = opened.borrow().clone().expect("harness opened");
            let session_path = std::path::PathBuf::from(micro.view.current().session.path);
            // An independent view of the root conversation: the stop must close it.
            let ctx = &*BACKGROUND_CONTEXT;
            let probe = harness
                .root(ctx)
                .await
                .expect("root")
                .watch(ctx)
                .await
                .expect("probe watch");

            let controller = micro.controller.clone();
            tokio::task::spawn_local(async move { controller.prompt("run the tool".into()).await });
            within("the tool call to wait in its beforeTool hook", waiting_rx)
                .await
                .expect("tool waiting");
            let bytes_at_loss = session_bytes(&session_path);
            assert!(!probe.closed(), "views stay open until the loss");
            assert!(
                harness.root(ctx).await.is_ok(),
                "session files stay open until the loss"
            );

            reclaim(&session_path);
            let lock = micro.view.session_lock().expect("host lock");
            assert!(lock.assert_held().is_err());

            within(
                "micro to mark the session fatal",
                view_reaches(&micro, |view| view.fatal.is_some()),
            )
            .await;
            within("the refused stop to be logged", stop_logged_rx)
                .await
                .expect("stop logged");
            assert!(
                probe.closed(),
                "the stop path closes the views without micro.close()"
            );
            // A refused in-flight write may fault the session first, which `read` reports ahead
            // of the close; either way nothing reaches the session files any more.
            assert!(
                matches!(
                    harness.root(ctx).await,
                    Err(Error::Closed | Error::Faulted(_))
                ),
                "the stop path closes the session files without micro.close()"
            );
            let (_, detail) = logged
                .borrow()
                .iter()
                .find(|(header, _)| header.starts_with("micro session stop"))
                .cloned()
                .expect("stop entry");
            let parsed: serde_json::Value = serde_json::from_str(&detail).expect("json detail");
            assert_eq!(parsed["event"], "micro_lost_lock_stop_failed");
            assert!(!detail.contains(&*session_path.to_string_lossy()));
            assert_eq!(session_bytes(&session_path), bytes_at_loss);

            within("micro to close", micro.close()).await;
            assert_eq!(session_bytes(&session_path), bytes_at_loss);
        })
        .await;
}
