use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use maho_ext_api::{BusSubscription, ComponentLogger, ExtensionApi, JsonValue};
use senpi_desktop_protocol::{PermissionDeniedData, TccPermission};
use serde::{Deserialize, Serialize};

pub const ROOT_PERMISSION_EVENT: &str = "omo.computer.permission_required";
pub const CHILD_PERMISSION_EVENT: &str = "computer.permission_required";
pub const TASK_CHILD_EXTENSION_EVENT: &str = "omo.task.child_extension_event";

pub const OMO_SENPI_TASK_RPC_CHILD: &str = "OMO_SENPI_TASK_RPC_CHILD";
pub const OMO_SENPI_TASK_DEPTH: &str = "OMO_SENPI_TASK_DEPTH";
pub const OMO_SENPI_TASK_ROOT_SESSION_ID: &str = "OMO_SENPI_TASK_ROOT_SESSION_ID";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionAncestry {
    pub depth: u32,
    pub root_session_id: Option<String>,
}

pub fn read_session_ancestry(
    session_context: Option<&BTreeMap<String, String>>,
    env: &BTreeMap<String, String>,
) -> Option<SessionAncestry> {
    if let Some(ctx) = session_context
        && let Some(role) = ctx.get("role")
        && !role.is_empty() {
        let depth = ctx
            .get("depth")
            .and_then(|d| d.parse::<u32>().ok())
            .unwrap_or(1);
        let root_session_id = ctx.get("root_session_id").cloned();
        return Some(SessionAncestry {
            depth: if depth >= 1 { depth } else { 1 },
            root_session_id,
        });
    }

    if env.get(OMO_SENPI_TASK_RPC_CHILD).map(|s| s.as_str()) == Some("1") {
        let depth = env
            .get(OMO_SENPI_TASK_DEPTH)
            .and_then(|d| d.parse::<u32>().ok())
            .unwrap_or(1);
        let root_session_id = env.get(OMO_SENPI_TASK_ROOT_SESSION_ID).cloned();
        return Some(SessionAncestry {
            depth: if depth >= 1 { depth } else { 1 },
            root_session_id,
        });
    }

    None
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ChildExtensionEvent {
    pub r#type: String,
    pub permission: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
}

pub fn parse_child_extension_event(value: &JsonValue) -> Option<ChildExtensionEvent> {
    let obj = value.as_object()?;
    let event_type = obj.get("type")?.as_str()?;
    if event_type != CHILD_PERMISSION_EVENT {
        return None;
    }
    let permission = obj.get("permission")?.as_str()?;
    if permission != "screen_recording" && permission != "accessibility" {
        return None;
    }
    let app = obj.get("app").and_then(JsonValue::as_str).map(ToString::to_string);
    Some(ChildExtensionEvent {
        r#type: event_type.to_string(),
        permission: permission.to_string(),
        app,
    })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForwardedChildEvent {
    pub parent_session_id: String,
    pub root_session_id: String,
    pub event: JsonValue,
}

pub fn parse_forwarded_child_event(value: &JsonValue) -> Option<ForwardedChildEvent> {
    let obj = value.as_object()?;
    let parent_session_id = obj.get("parent_session_id")?.as_str()?.to_string();
    let root_session_id = obj.get("root_session_id")?.as_str()?.to_string();
    let event = obj.get("event")?.clone();
    Some(ForwardedChildEvent {
        parent_session_id,
        root_session_id,
        event,
    })
}

pub type ReportPermissionFn = Arc<dyn Fn(PermissionDeniedData) + Send + Sync>;
type InternalReportFn = Arc<dyn Fn(String, Option<String>, Option<Arc<dyn ComponentLogger>>) + Send + Sync>;

pub fn wire_computer_permission_events(
    api: &mut ExtensionApi,
    env: &BTreeMap<String, String>,
) -> ReportPermissionFn {
    let latches = Arc::new(Mutex::new(std::collections::HashSet::<(String, String)>::new()));
    let current_session_id = Arc::new(Mutex::new(Option::<String>::None));
    let unsubscribe_slot: Arc<Mutex<Option<BusSubscription>>> = Arc::new(Mutex::new(None));
    let env_captured = env.clone();

    let sess_id_clone = Arc::clone(&current_session_id);

    let sess_id_reporter = Arc::clone(&current_session_id);
    let api_events = api.events.clone();

    let api_appender = {
        let loaded = api.registered.clone();
        let profile = api.profile.clone();
        let events = api.events.clone();
        let runtime = api.runtime.clone();
        Arc::new(move || {
            ExtensionApi::new(loaded.clone(), profile.clone(), events.clone(), runtime.clone())
        })
    };

    let logger_slot: Arc<Mutex<Option<Arc<dyn ComponentLogger>>>> = Arc::new(Mutex::new(None));

    let report_fn: InternalReportFn = {
        let api_events = api_events.clone();
        let sess_id_reporter = Arc::clone(&sess_id_reporter);
        let env_captured = env_captured.clone();
        let api_appender = Arc::clone(&api_appender);
        let logger_slot_reporter = Arc::clone(&logger_slot);
        let latches_clone = Arc::clone(&latches);

        Arc::new(move |permission: String, app: Option<String>, logger: Option<Arc<dyn ComponentLogger>>| {
            let sid = sess_id_reporter.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let Some(session_id) = sid else {
                return;
            };

            if read_session_ancestry(None, &env_captured).is_some() {
                let mut event_obj = serde_json::Map::new();
                event_obj.insert("type".to_string(), JsonValue::String(CHILD_PERMISSION_EVENT.to_string()));
                event_obj.insert("permission".to_string(), JsonValue::String(permission));
                if let Some(app_name) = app {
                    event_obj.insert("app".to_string(), JsonValue::String(app_name));
                }
                let payload = JsonValue::Object(event_obj);
                api_events.emit(CHILD_PERMISSION_EVENT, &payload);
                return;
            }

            {
                let mut map = latches_clone.lock().unwrap_or_else(|e| e.into_inner());
                let key = (session_id.clone(), permission.clone());
                if map.contains(&key) {
                    return;
                }
                map.insert(key);
            }

            let mut data = serde_json::Map::new();
            data.insert("session_id".to_string(), JsonValue::String(session_id.clone()));
            data.insert("permission".to_string(), JsonValue::String(permission.clone()));
            if let Some(app_name) = app {
                data.insert("app".to_string(), JsonValue::String(app_name));
            }
            let payload = JsonValue::Object(data);
            api_events.emit(ROOT_PERMISSION_EVENT, &payload);

            let mut entry = serde_json::Map::new();
            entry.insert("session_id".to_string(), JsonValue::String(session_id));
            entry.insert("permission".to_string(), JsonValue::String(permission));
            let api_inst = api_appender();
            if let Err(e) = api_inst.append_entry(ROOT_PERMISSION_EVENT, Some(JsonValue::Object(entry))) {
                let active_logger = logger.or_else(|| logger_slot_reporter.lock().unwrap_or_else(|e| e.into_inner()).clone());
                if let Some(l) = active_logger {
                    l.warn(
                        &format!("Computer permission event emitted, but its session marker could not be persisted: {e}"),
                        None,
                    );
                }
            }
        })
    };

    let logger_slot_for_start = Arc::clone(&logger_slot);

    let setup_child_listener = {
        let events_bus = api.events.clone();
        let report_inner = Arc::clone(&report_fn);
        let sess_inner = Arc::clone(&current_session_id);
        let env_inner = env_captured.clone();
        let unsub = Arc::clone(&unsubscribe_slot);

        Arc::new(move || {
            let report_inner = Arc::clone(&report_inner);
            let sess_inner = Arc::clone(&sess_inner);
            let env_inner = env_inner.clone();

            let sub = events_bus.on(
                TASK_CHILD_EXTENSION_EVENT,
                Arc::new(move |value| {
                    let Some(forwarded) = parse_forwarded_child_event(value) else {
                        return;
                    };
                    let current_id = sess_inner.lock().unwrap_or_else(|e| e.into_inner()).clone();
                    let Some(session_id) = current_id else {
                        return;
                    };
                    if forwarded.parent_session_id != session_id {
                        return;
                    }

                    let root = read_session_ancestry(None, &env_inner)
                        .and_then(|a| a.root_session_id)
                        .unwrap_or_else(|| session_id.clone());

                    if forwarded.root_session_id != root {
                        return;
                    }

                    if let Some(child_event) = parse_child_extension_event(&forwarded.event) {
                        report_inner(child_event.permission, child_event.app, None);
                    }
                }),
            );
            *unsub.lock().unwrap_or_else(|e| e.into_inner()) = Some(sub);
        })
    };

    let sess_raw = Arc::clone(&current_session_id);
    let setup_child_on_raw = Arc::clone(&setup_child_listener);
    let raw_sub_start = api.events.on("session_start", Arc::new(move |payload| {
        let sid = payload
            .get("session_id")
            .or_else(|| payload.get("sessionId"))
            .and_then(|v| v.as_str());
        if let Some(id) = sid {
            *sess_raw.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_string());
            setup_child_on_raw();
        }
    }));
    std::mem::forget(raw_sub_start);

    let sess_raw_shut = Arc::clone(&current_session_id);
    let unsub_on_raw = Arc::clone(&unsubscribe_slot);
    let raw_sub_shut = api.events.on("session_shutdown", Arc::new(move |_payload| {
        *sess_raw_shut.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *unsub_on_raw.lock().unwrap_or_else(|e| e.into_inner()) = None;
    }));
    std::mem::forget(raw_sub_shut);

    let setup_child_on_evt = Arc::clone(&setup_child_listener);
    api.on(
        maho_ext_api::EventKind::SessionStart,
        Arc::new(move |_event, ctx| {
            let id = ctx.session_manager.session_id().to_string();
            if id.is_empty() {
                return Box::pin(async { Ok(maho_ext_api::EventResult::None) });
            }
            *sess_id_clone.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.clone());
            *logger_slot_for_start.lock().unwrap_or_else(|e| e.into_inner()) = ctx.logger.clone();

            setup_child_on_evt();

            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        }),
    );

    let sess_id_clone = Arc::clone(&current_session_id);
    let unsub_clone = Arc::clone(&unsubscribe_slot);
    let logger_slot_for_shutdown = Arc::clone(&logger_slot);
    api.on(
        maho_ext_api::EventKind::SessionShutdown,
        Arc::new(move |_event, _ctx| {
            *sess_id_clone.lock().unwrap_or_else(|e| e.into_inner()) = None;
            *logger_slot_for_shutdown.lock().unwrap_or_else(|e| e.into_inner()) = None;
            let mut slot = unsub_clone.lock().unwrap_or_else(|e| e.into_inner());
            *slot = None;
            Box::pin(async { Ok(maho_ext_api::EventResult::None) })
        }),
    );

    let report_clone = Arc::clone(&report_fn);
    Arc::new(move |perm_data: PermissionDeniedData| {
        let perm_str = match perm_data.permission {
            TccPermission::ScreenRecording => "screen_recording",
            TccPermission::Accessibility => "accessibility",
        };
        let app = if perm_data.app.is_empty() {
            None
        } else {
            Some(perm_data.app.clone())
        };
        report_clone(perm_str.to_string(), app, None);
    })
}
