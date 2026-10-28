//! The `<builtin:gateway>` extension: endpoint registration and inbox polling for long-lived
//! sessions, and the `session_list` / `session_send` tools.

use crate::message::Receiver;
use crate::settings::Settings;
use crate::store::{Endpoint, Store, VERSION, now};
use crate::tools::{self, Caller, LIST_TOOL, SEND_TOOL, ToolOutcome};
use maho_ext_api::{
    AgentToolResult, EventKind, EventResult, Extension, ExtensionApi, ExtensionContext,
    ExtensionEvent, ExtensionMode, InputSource, ToolDefinition, ToolError,
};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// The gateway extension. `root: None` uses `<agent dir>/gateway` (or `$MAHO_GATEWAY_DIR`) of the
/// session; `settings: None` loads them from the session's `omo.jsonc` and environment.
#[derive(Clone, Debug, Default)]
pub struct GatewayExtension {
    pub root: Option<PathBuf>,
    pub settings: Option<Settings>,
}

/// Whether sessions of `mode` register an endpoint and receive deliveries (long-lived ones only).
pub fn receives_in(mode: ExtensionMode) -> bool {
    matches!(
        mode,
        ExtensionMode::Tui | ExtensionMode::Rpc | ExtensionMode::AppServer
    )
}

struct Poller {
    session_id: String,
    stop: Arc<AtomicBool>,
}

struct State {
    store: Mutex<Option<Store>>,
    settings: Settings,
    hop: Arc<AtomicU32>,
    poller: Mutex<Option<Poller>>,
    sender: Arc<ExtensionApi>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl State {
    fn store_for(&self, ctx: &ExtensionContext) -> Store {
        lock(&self.store)
            .get_or_insert_with(|| {
                Store::new(crate::store::default_root(&ctx.agent_dir, &|key| {
                    std::env::var(key).ok()
                }))
            })
            .clone()
    }

    fn stop(&self) {
        if let Some(poller) = lock(&self.poller).take() {
            poller.stop.store(true, Ordering::SeqCst);
            if let Some(store) = lock(&self.store).as_ref() {
                store.unregister(&poller.session_id);
            }
        }
    }

    fn start(&self, store: Store, session_id: String, cwd: String) {
        if lock(&self.poller)
            .as_ref()
            .is_some_and(|poller| poller.session_id == session_id)
        {
            return;
        }
        self.stop();
        let endpoint = Endpoint {
            version: VERSION,
            session_id: session_id.clone(),
            name: self.sender.get_session_name().ok().flatten(),
            cwd,
            pid: std::process::id(),
            started_at: now(),
        };
        if let Err(error) = store.register(&endpoint) {
            maho_ext_api::diagnostic(&format!("gateway: cannot register the session: {error}"));
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        *lock(&self.poller) = Some(Poller {
            session_id: session_id.clone(),
            stop: Arc::clone(&stop),
        });
        let sender = Arc::clone(&self.sender);
        let hop = Arc::clone(&self.hop);
        let interval = self.settings.poll;
        let handle = tokio::runtime::Handle::try_current().ok();
        let spawned = std::thread::Builder::new()
            .name("mhc-gateway".into())
            .spawn(move || {
                let _entered = handle.as_ref().map(tokio::runtime::Handle::enter);
                let receiver = Receiver::new();
                let send = |message, options| {
                    sender
                        .send_message(message, options)
                        .map_err(|error| error.to_string())
                };
                while !stop.load(Ordering::SeqCst) {
                    let outcome = receiver.poll_once(&store, &session_id, &send);
                    if let Some((_, last)) = outcome.delivered.last() {
                        hop.store(*last, Ordering::SeqCst);
                    }
                    std::thread::sleep(interval);
                }
            });
        if let Err(error) = spawned {
            maho_ext_api::diagnostic(&format!("gateway: cannot start the inbox poller: {error}"));
        }
    }
}

fn definition(
    name: &str,
    label: &str,
    description: &str,
    parameters: serde_json::Value,
) -> ToolDefinition {
    let mut definition = ToolDefinition::new(
        name,
        description,
        parameters,
        Arc::new(|_| {
            Box::pin(async { Err(ToolError::Message("Extension context required".into())) })
        }),
    );
    label.clone_into(&mut definition.label);
    definition
}

fn to_result(outcome: ToolOutcome) -> AgentToolResult {
    let mut result = AgentToolResult::text(outcome.text);
    result.details = outcome.details;
    if outcome.is_error {
        result.is_error = Some(true);
    }
    result
}

impl Extension for GatewayExtension {
    fn register(&self, api: &mut ExtensionApi) {
        let settings = self.settings.unwrap_or_else(|| Settings::load(&api.cwd));
        if !settings.enabled {
            return;
        }
        let state = Arc::new(State {
            store: Mutex::new(self.root.clone().map(Store::new)),
            settings,
            hop: Arc::new(AtomicU32::new(0)),
            poller: Mutex::new(None),
            sender: Arc::new(ExtensionApi::new(
                api.registered.clone(),
                api.profile.clone(),
                api.events.clone(),
                api.runtime.clone(),
            )),
        });

        let list_state = Arc::clone(&state);
        let mut list = definition(
            LIST_TOOL,
            "Sessions",
            "List the other mhc sessions that are running now and can receive messages through the gateway (id, name, working directory, start time).",
            json!({"type": "object", "properties": {}}),
        );
        list.prompt_snippet = Some("List other running mhc sessions".into());
        if let Err(error) = api.register_tool_with_extension_context(
            list,
            Arc::new(move |_id, _params, _signal, _update, ctx| {
                let state = Arc::clone(&list_state);
                Box::pin(async move {
                    let store = state.store_for(ctx);
                    Ok(to_result(tools::list(
                        &store,
                        ctx.session_manager.session_id(),
                    )))
                })
            }),
        ) {
            maho_ext_api::diagnostic(&format!("gateway: {error}"));
        }

        let send_state = Arc::clone(&state);
        let mut send = definition(
            SEND_TOOL,
            "Send to session",
            "Send a message to another running mhc session (see session_list). The receiver sees it labelled with this session as the sender. mode `followUp` (default) queues it after the receiver's current turn; `steer` interrupts that turn.",
            json!({
                "type": "object",
                "properties": {
                    "to": {"type": "string", "description": "Target session: id, name, or unique id prefix (4+ characters)"},
                    "message": {"type": "string", "description": "The message text"},
                    "mode": {"type": "string", "enum": ["followUp", "steer"], "description": "Delivery mode (default followUp)"}
                },
                "required": ["to", "message"]
            }),
        );
        send.prompt_snippet = Some("Message another running mhc session".into());
        if let Err(error) = api.register_tool_with_extension_context(
            send,
            Arc::new(move |_id, params, _signal, _update, ctx| {
                let state = Arc::clone(&send_state);
                Box::pin(async move {
                    let store = state.store_for(ctx);
                    let caller = Caller {
                        session_id: ctx.session_manager.session_id().to_owned(),
                        name: state.sender.get_session_name().ok().flatten(),
                        hop: state.hop.load(Ordering::SeqCst),
                    };
                    Ok(to_result(tools::send(
                        &store,
                        &caller,
                        state.settings.max_hops,
                        &params,
                    )))
                })
            }),
        ) {
            maho_ext_api::diagnostic(&format!("gateway: {error}"));
        }

        let start_state = Arc::clone(&state);
        api.on(
            EventKind::SessionStart,
            Arc::new(move |_event, ctx| {
                let state = Arc::clone(&start_state);
                Box::pin(async move {
                    if receives_in(ctx.mode) {
                        let store = state.store_for(ctx);
                        state.start(
                            store,
                            ctx.session_manager.session_id().to_owned(),
                            ctx.cwd.to_string_lossy().into_owned(),
                        );
                    }
                    Ok(EventResult::None)
                })
            }),
        );

        let stop_state = Arc::clone(&state);
        api.on(
            EventKind::SessionShutdown,
            Arc::new(move |_event, _ctx| {
                let state = Arc::clone(&stop_state);
                Box::pin(async move {
                    state.stop();
                    Ok(EventResult::None)
                })
            }),
        );

        let input_state = state;
        api.on(
            EventKind::Input,
            Arc::new(move |event, _ctx| {
                if let ExtensionEvent::Input(input) = event
                    && input.source != InputSource::Extension
                {
                    input_state.hop.store(0, Ordering::SeqCst);
                }
                Box::pin(async { Ok(EventResult::None) })
            }),
        );
    }
}
