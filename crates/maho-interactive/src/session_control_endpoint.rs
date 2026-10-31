//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-endpoint.ts (v2026.10.27-2).
//!
//! One terminal's control endpoint, from registration to clean exit. senpi loads it only when an
//! extension registers one; here it is plain code that runs nothing until then, so a plain TUI pays
//! nothing either.
//!
//! Order matters and each step is undone on failure, so a failed bind leaves no half-registered
//! directory: the socket is bound (secret 0600, directory 0700) while the session header is made
//! durable, and only once both are done is the endpoint registered - a visible endpoint is one that
//! answers, and its session id is already on disk. Nothing a sender needs waits on work it does not
//! need: the writer stamp's process lookup starts at entry, the inbox watch arms after registration
//! returned, and dead `tui` endpoints of other terminals are reaped after activation. Wakes come from
//! edges only (see `session_control_wake`).
use crate::session_control_attach::ControlAttachments;
use crate::session_control_commands::{ControlCommandContext, run_control_command};
use crate::session_control_feed::ControlFeed;
use crate::session_control_lifecycle::{
    ActivationParts, ActiveControlEndpoint, TuiControlContext, WakeFn, activate_control_endpoint,
    editor_hold,
};
use crate::session_control_registry::{
    register_tui_endpoint, resolve_tui_socket, unregister_tui_endpoint,
};
use crate::session_control_rpc_commands::RpcCommandContext;
use crate::session_control_server::{
    ControlCommand, ControlConnection, ControlServer, ControlServerHandlers, listen_control_socket,
};
use futures::future::BoxFuture;
use maho_core::extensions::session_control_types::{
    RegisterControlEndpointOptions, SessionControlRegistration,
};
use maho_rpc::session_control_wake::{WakeScheduler, watch_inbox};
use maho_rpc::socket_transport::{create_socket_secret, socket_secret_path};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub enum ControlEndpointOutcome {
    Registered {
        registration: SessionControlRegistration,
        endpoint: ActiveControlEndpoint,
    },
    NotRegistered {
        registration: SessionControlRegistration,
    },
}

type Cleanup = BoxFuture<'static, ()>;

pub async fn register_session_control_endpoint(
    context: TuiControlContext,
    options: RegisterControlEndpointOptions,
) -> ControlEndpointOutcome {
    let mut cleanups: Vec<Cleanup> = Vec::new();
    match open_endpoint(&context, options, &mut cleanups).await {
        Ok(endpoint) => {
            let dispose_endpoint = endpoint.clone();
            ControlEndpointOutcome::Registered {
                registration: SessionControlRegistration::Registered {
                    socket: endpoint.socket.clone(),
                    dispose: Arc::new(move || -> BoxFuture<'static, Result<(), String>> {
                        let teardown = dispose_endpoint.dispose();
                        Box::pin(async move {
                            teardown.await;
                            Ok(())
                        })
                    }),
                },
                endpoint,
            }
        }
        Err(reason) => {
            while let Some(cleanup) = cleanups.pop() {
                cleanup.await;
            }
            context
                .surface
                .notice(&format!("control endpoint unavailable: {reason}"));
            ControlEndpointOutcome::NotRegistered {
                registration: SessionControlRegistration::Failed { reason },
            }
        }
    }
}

fn panic_reason(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|reason| (*reason).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "drain panicked".to_owned())
}

fn remove_file(path: String) -> Cleanup {
    Box::pin(async move {
        drop(std::fs::remove_file(path));
    })
}

struct EndpointHandlers {
    commands: Arc<ControlCommandContext>,
    attachments: Arc<ControlAttachments>,
    feed: Arc<Mutex<ControlFeed>>,
    context: TuiControlContext,
}

impl ControlServerHandlers for EndpointHandlers {
    fn command(
        &self,
        connection: ControlConnection,
        command: ControlCommand,
    ) -> BoxFuture<'static, Option<Value>> {
        let commands = self.commands.clone();
        Box::pin(async move { run_control_command(&commands, &connection, &command).await })
    }

    fn opened(&self, connection: &ControlConnection) {
        self.attachments.opened(connection);
    }

    fn respond(&self, connection: &ControlConnection, answer: &Value) -> bool {
        self.attachments.respond(connection, answer)
    }

    fn closed(&self, connection: &ControlConnection) {
        self.feed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .unsubscribe(connection.id);
        self.attachments.closed(connection);
    }

    fn listener_error(&self, error: String) {
        self.context
            .surface
            .notice(&format!("control endpoint listener failed: {error}"));
    }
}

async fn open_endpoint(
    context: &TuiControlContext,
    options: RegisterControlEndpointOptions,
    cleanups: &mut Vec<Cleanup>,
) -> Result<ActiveControlEndpoint, String> {
    let TuiControlContext {
        session,
        agent_dir,
        surface,
        ..
    } = context;
    // The writer stamp's process lookup is a process constant: started here, it overlaps the header
    // write and the bind instead of running inside the registry lock.
    drop(maho_rpc::host_daemon_registration::this_process_start_time());
    // Bound while the header is written, awaited before the endpoint is registered.
    // senpi `sessionManager.persistHeaderNow()`: the manager writes its header and records the file as
    // the one this process created, which the header-only rule may later remove.
    let header_session = session.clone();
    let header_durable = tokio::task::spawn_blocking(move || {
        header_session.with_session_manager_mut(|manager| manager.persist_header_now())
    });
    let instance_id = uuid::Uuid::new_v4().to_string();
    let socket = resolve_tui_socket(agent_dir, &instance_id)?;
    let secret_path = socket_secret_path(&socket)?;
    cleanups.push(remove_file(socket.clone()));
    cleanups.push(remove_file(secret_path.clone()));
    let secret = create_socket_secret(std::path::Path::new(&secret_path))
        .map_err(|error| error.to_string())?;
    let feed = Arc::new(Mutex::new(ControlFeed::new()));
    // Every wake runs a pass, so a failure that repeats is shown once, until a pass succeeds again.
    let failing = Arc::new(Mutex::new(None::<String>));
    let drain = options.drain.clone();
    let pass_failing = failing.clone();
    let error_failing = failing.clone();
    let error_surface = surface.clone();
    let wake_session = session.clone();
    let scheduler = WakeScheduler::new(
        Arc::new(
            move |event: Value| -> BoxFuture<'static, Result<Value, String>> {
                let drain = drain.clone();
                let failing = pass_failing.clone();
                let session = wake_session.clone();
                Box::pin(async move {
                    // senpi emits the wake into the session's extensions first
                    // (`extensionRunner.emit(event)`); handler failures go to the runner's error
                    // listeners, and only an inactive runner fails the pass.
                    session.emit_session_control_wake(&event).await?;
                    // senpi's drain may throw: an `Err` is the failed pass. A panicking drain fails
                    // the pass too, and must not take the scheduler's task down with it.
                    let result = futures::FutureExt::catch_unwind(std::panic::AssertUnwindSafe(
                        async move { drain(event).await },
                    ))
                    .await
                    .map_err(|panic| panic_reason(panic.as_ref()))??
                    .unwrap_or_else(|| json!({"admitted": []}));
                    *failing
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
                    Ok(result)
                })
            },
        ),
        Arc::new(move |reason: String| {
            let mut failing = error_failing
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if failing.as_deref() == Some(reason.as_str()) {
                return;
            }
            *failing = Some(reason.clone());
            drop(failing);
            error_surface.notice(&format!("control endpoint drain failed: {reason}"));
        }),
    );
    let disposed_scheduler = scheduler.clone();
    cleanups.push(Box::pin(async move { disposed_scheduler.dispose() }));
    let wake_scheduler = scheduler.clone();
    let wake: WakeFn = Arc::new(move |reason: &str| wake_scheduler.request(reason, &[]));
    let question_surface = surface.clone();
    let attachments = Arc::new(ControlAttachments::new(
        session.clone(),
        Arc::new(move || question_surface.pending_question_ids().len()),
    ));
    let disposed_attachments = attachments.clone();
    cleanups.push(Box::pin(async move { disposed_attachments.dispose() }));
    let command_scheduler = scheduler.clone();
    let hold_context = context.clone();
    // The effect ledger's view of this session, and the session events its ledgers react to; both go
    // with the endpoint (senpi keys the ledgers by the session object).
    let effect_route = format!("tui:{instance_id}");
    let effect_session: Arc<dyn maho_rpc::effect_session::EffectSession> =
        Arc::new(maho_rpc::agent_effect_session::AgentEffectSession::new(
            (**session).clone(),
            effect_route.clone(),
        ));
    let effect_events = Arc::new(Mutex::new(Some(
        maho_rpc::agent_effect_session::AgentEffectSession::observe(
            session,
            effect_session.clone(),
        ),
    )));
    let release_effects: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        drop(
            effect_events
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        maho_rpc::client_admissions::ClientAdmissions::forget(&effect_route);
        maho_rpc::effect_ledger::EffectLedger::forget(&effect_route);
    });
    let released_effects = release_effects.clone();
    cleanups.push(Box::pin(async move { released_effects() }));
    let commands = Arc::new(ControlCommandContext {
        instance_id: instance_id.clone(),
        session: session.clone(),
        surface: surface.clone(),
        feed: feed.clone(),
        wake: Arc::new(move |ids: Vec<String>| command_scheduler.request("command", &ids)),
        attachments: attachments.clone(),
        rpc: RpcCommandContext {
            instance_id: instance_id.clone(),
            session: session.clone(),
            attachments: attachments.clone(),
            editor_hold: Arc::new(move || editor_hold(&hold_context)),
            effect_session: Some(effect_session),
        },
    });
    let server: Arc<ControlServer> = Arc::new(
        listen_control_socket(
            &socket,
            secret,
            Arc::new(EndpointHandlers {
                commands,
                attachments,
                feed: feed.clone(),
                context: context.clone(),
            }),
        )
        .await?,
    );
    let closed_server = server.clone();
    cleanups.push(Box::pin(async move { closed_server.close().await }));
    header_durable.await.map_err(|error| error.to_string())??;
    let entry = register_tui_endpoint(agent_dir, &socket, &instance_id).await?;
    let unregistered = entry.clone();
    cleanups.push(Box::pin(async move {
        unregister_tui_endpoint(&unregistered).await;
    }));
    let inbox_wake = wake.clone();
    let inbox_surface = surface.clone();
    let inbox = watch_inbox(
        std::path::Path::new(&options.inbox_dir),
        // Called on the watcher's own thread (no runtime): the scheduler spawns the pass on its runtime itself.
        Arc::new(move || drop(inbox_wake("inbox"))),
        Arc::new(move |error: String| {
            inbox_surface.notice(&format!("control endpoint inbox watch failed: {error}"));
        }),
    )
    .await?;
    let stop_inbox = inbox.stopper();
    let stopped_inbox = stop_inbox.clone();
    cleanups.push(Box::pin(async move { stopped_inbox() }));
    let endpoint = activate_control_endpoint(ActivationParts {
        context: context.clone(),
        options,
        entry,
        server,
        scheduler,
        feed,
        stop_inbox,
        release_effects,
        wake,
    });
    // Anything that reached the inbox before this point is picked up by this first pass, and
    // anything written after it but before the watch was armed (no event for it) by the pass once
    // arming settled.
    endpoint.wake("inbox");
    let armed_endpoint = endpoint.clone();
    let armed = inbox.armed;
    tokio::spawn(async move {
        armed.await;
        armed_endpoint.wake("inbox");
    });
    // Other terminals' dead records are nothing a sender reads: reaped after activation, off the path.
    let gc_dir = agent_dir.clone();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        maho_rpc::host_gc::gc_host_endpoints(
            std::path::Path::new(&gc_dir),
            &maho_rpc::host_gc::HostGcOptions {
                kinds: Some(vec![maho_rpc::host_endpoints::EndpointKind::Tui]),
            },
        )
        .await;
    });
    Ok(endpoint)
}
