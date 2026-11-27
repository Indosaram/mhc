//! Shared internal helpers for thread tools (`thread/tools/internals.ts`).

use std::collections::HashSet;
use std::sync::Arc;

use serde_json::{Value, json};

use super::super::address_book::{
    AddressBookHost, AddressEntry, DiskSession, FindDiskSessionsOptions, HostListSessions,
    HostSession, HostSessionStatus, assemble_address_book, find_disk_sessions_with_options,
    to_thread_address_entries,
};
use super::super::addressing::{
    ThreadAddressEntry, ThreadResolveResult, resolve_target, workspace_directories,
};
use super::super::contracts::results::ThreadRetryWait;
use super::super::contracts::state::{ThreadActivity, ThreadModel, ThreadState};
use super::super::errors::thread_tool_failure;
use super::super::metadata::{THREAD_TOOL_METADATA, ThreadToolEntry};
use super::super::session_facts::thread_title;
use super::super::thread_link::thread_link;
use super::ports::{
    ThreadHostSession, ThreadHostView, ThreadHostViewRequest, ThreadSessionPort,
    ThreadToolSurfaceOptions,
};
use super::thread_state::{activity_detail, retry_wait, thread_state};

pub fn failure(
    code: &str,
    message: impl Into<String>,
    next: impl Into<String>,
    details: Option<Value>,
) -> Value {
    json!({
        "kind": "error",
        "error": thread_tool_failure(code, message, next, details)
    })
}

pub fn uncertain_failure(note: Option<&str>) -> Value {
    failure(
        "idempotency_uncertain",
        "The earlier operation may have been delivered.",
        "Read the target transcript before deciding whether to retry.",
        note.map(|n| json!({ "error_note": n })),
    )
}

#[derive(Debug, Clone)]
pub struct ThreadToolSummary {
    pub thread_id: String,
    pub session_id: String,
    pub durable_session_id: Option<String>,
    pub session_path: Option<String>,
    pub socket: Option<String>,
    pub name: String,
    pub unnamed: Option<bool>,
    pub status: String,
    pub state: ThreadState,
    pub activity: Option<ThreadActivity>,
    pub error_code: Option<String>,
    pub turn_id: Option<String>,
    pub waiting_on: Option<ThreadRetryWait>,
    pub model: Option<ThreadModel>,
    pub as_of: Option<String>,
    pub device: Option<String>,
    pub alias: Option<String>,
    pub cwd: String,
    pub created_at: String,
    pub updated_at: Option<String>,
    pub surface: Option<String>,
    pub controls: Option<Vec<String>>,
    pub engine_version: Option<String>,
    pub error_note: Option<String>,
    pub skills: Option<Vec<String>>,
    pub link: String,
}

pub fn metadata_entry(name: &str) -> Option<&'static ThreadToolEntry> {
    THREAD_TOOL_METADATA.iter().find(|entry| entry.name == name)
}

pub fn summary(session: &ThreadHostSession, entry: Option<&AddressEntry>) -> ThreadToolSummary {
    let id = session
        .durable_session_id
        .as_deref()
        .unwrap_or(&session.session_id)
        .to_owned();
    let created = entry
        .map(|e| e.created_at.clone())
        .or_else(|| session.created_at.clone())
        .unwrap_or_else(|| chrono::Utc::now().to_rfc3339());
    let held = session.status.as_deref() != Some("closed") && entry.is_none_or(|e| e.alive);
    let title = entry
        .and_then(|e| e.title.clone())
        .or_else(|| thread_title(session.name.as_deref(), None))
        .unwrap_or_default();
    let act = activity_detail(
        held && session.archived != Some(true),
        session.activity,
        session.error_code.as_deref(),
        session.turn_id.as_deref(),
    );
    let wait = retry_wait(
        held && session.archived != Some(true) && session.activity == Some(ThreadActivity::Running),
        session.waiting.as_ref(),
    );
    let model = session.model.as_ref().and_then(|m| {
        if m.provider.is_empty() || m.id.is_empty() {
            None
        } else {
            Some(ThreadModel {
                provider: m.provider.clone(),
                id: m.id.clone(),
            })
        }
    });
    ThreadToolSummary {
        thread_id: id.clone(),
        session_id: session.session_id.clone(),
        durable_session_id: session.durable_session_id.clone(),
        session_path: session.session_path.clone(),
        socket: session.socket.clone(),
        name: title.clone(),
        unnamed: None,
        status: if held { "live" } else { "resumable" }.to_owned(),
        state: thread_state(session.archived, held, session.activity),
        activity: act.activity,
        error_code: act.error_code,
        turn_id: act.turn_id,
        waiting_on: wait,
        model,
        as_of: None,
        device: None,
        alias: None,
        cwd: session.cwd.clone(),
        created_at: created.clone(),
        updated_at: match entry {
            Some(e) => e.updated_at.clone(),
            None => session.updated_at.clone().or(Some(created)),
        },
        surface: entry
            .and_then(|e| e.surface.map(|s| s.as_str().to_owned()))
            .or_else(|| {
                session.endpoint_kind.map(|k| {
                    super::super::address_book::derive_surface(
                        Some(super::super::address_book::to_gateway_endpoint_kind(k)),
                        session.kind.as_deref(),
                        session.socket.as_deref().unwrap_or(""),
                    )
                    .as_str()
                    .to_owned()
                })
            }),
        controls: session
            .controls
            .as_ref()
            .map(|c| c.iter().map(|ctrl| ctrl.as_str().to_owned()).collect()),
        engine_version: session.engine_version.clone(),
        error_note: None,
        skills: None,
        link: thread_link(
            &id,
            entry
                .and_then(|e| e.title.as_deref())
                .or(session.name.as_deref()),
        ),
    }
}

pub fn degraded_summary(entry: &AddressEntry) -> ThreadToolSummary {
    ThreadToolSummary {
        thread_id: entry.thread_id.clone(),
        session_id: entry.durable_id.clone(),
        durable_session_id: Some(entry.durable_id.clone()),
        session_path: entry.session_path.clone(),
        socket: entry.source_host.clone(),
        name: entry.title.clone().unwrap_or_default(),
        unnamed: None,
        status: "resumable".to_owned(),
        state: thread_state(Some(entry.archived), false, None),
        activity: None,
        error_code: None,
        turn_id: None,
        waiting_on: None,
        model: None,
        as_of: None,
        device: None,
        alias: None,
        cwd: entry.cwd.clone(),
        created_at: entry.created_at.clone(),
        updated_at: entry.updated_at.clone(),
        surface: entry.surface.map(|s| s.as_str().to_owned()),
        controls: None,
        engine_version: None,
        error_note: entry.error_note.clone(),
        skills: None,
        link: thread_link(&entry.thread_id, entry.title.as_deref()),
    }
}

fn to_host_session(session: &ThreadHostSession) -> HostSession {
    HostSession {
        session_id: session.session_id.clone(),
        durable_session_id: session.durable_session_id.clone(),
        session_path: session.session_path.clone(),
        cwd: session.cwd.clone(),
        name: session.name.clone(),
        status: session.status.as_deref().and_then(HostSessionStatus::parse),
        kind: session.kind.clone(),
        created_at: session.created_at.clone(),
        updated_at: session.updated_at.clone(),
    }
}

pub async fn host_view(
    options: &ThreadToolSurfaceOptions,
    request: ThreadHostViewRequest,
) -> Result<ThreadHostView, String> {
    if let Some(view) = options.host.list_view(request).await? {
        return Ok(view);
    }
    match options.host.list_sessions().await {
        Ok(sessions) => {
            let socket = options.host.socket().to_owned();
            let host_sessions: Vec<HostSession> = sessions.iter().map(to_host_session).collect();
            let kind = super::super::address_book::to_gateway_endpoint_kind(
                super::super::endpoint_registry::endpoint_kind_of(&socket, &[]),
            );
            Ok(ThreadHostView {
                hosts: vec![AddressBookHost {
                    socket,
                    list_sessions: Some(HostListSessions::ok(host_sessions)),
                    error: None,
                    endpoint_kind: kind,
                    alive: Some(true),
                    reason: None,
                    legacy: true,
                }],
                sessions,
                disk: Vec::new(),
            })
        }
        Err(error) => {
            if !request.offline {
                return Err(error);
            }
            let socket = options.host.socket().to_owned();
            let kind = super::super::address_book::to_gateway_endpoint_kind(
                super::super::endpoint_registry::endpoint_kind_of(&socket, &[]),
            );
            Ok(ThreadHostView {
                sessions: Vec::new(),
                hosts: vec![AddressBookHost {
                    socket,
                    list_sessions: None,
                    error: Some(error),
                    endpoint_kind: kind,
                    alive: Some(false),
                    reason: None,
                    legacy: true,
                }],
                disk: Vec::new(),
            })
        }
    }
}

pub fn address_book(
    options: &ThreadToolSurfaceOptions,
    view: &ThreadHostView,
    extra: &[DiskSession],
) -> Vec<AddressEntry> {
    let mut disk = options
        .disk_sessions
        .as_ref()
        .map(|f| f())
        .unwrap_or_default();
    disk.extend(view.disk.iter().cloned());
    disk.extend(extra.iter().cloned());
    assemble_address_book(&view.hosts, &disk, "")
}

pub fn send_address_book(
    options: &ThreadToolSurfaceOptions,
    view: &ThreadHostView,
    address: &str,
    all_scope: bool,
) -> Vec<AddressEntry> {
    let book = address_book(options, view, &[]);
    let Some(sessions_dir) = options.sessions_directory.as_ref().map(|f| f()) else {
        return book;
    };
    if address == "self" {
        return book;
    }
    let known: HashSet<String> = book.iter().map(|e| e.durable_id.clone()).collect();
    if known.contains(address) {
        return book;
    }
    let root = (options.caller_workspace_root)();
    let resolved = resolve_target(
        &to_thread_address_entries(&book),
        address,
        &super::super::addressing::ThreadResolveOptions {
            all_scope,
            caller_workspace_root: Some(root.clone()),
            ..Default::default()
        },
    );
    let id_only = match resolved {
        ThreadResolveResult::Error(err) => err.code.as_str() != "not_found",
        ThreadResolveResult::Ok { .. } => true,
    };
    let found: Vec<DiskSession> = find_disk_sessions_with_options(
        &sessions_dir,
        address,
        &FindDiskSessionsOptions {
            all_scope,
            workspace_roots: workspace_directories(&root),
            id_only,
        },
    )
    .into_iter()
    .filter(|s| !known.contains(&s.durable_id))
    .collect();
    if found.is_empty() {
        book
    } else {
        address_book(options, view, &found)
    }
}

pub fn send_book_entries(
    options: &ThreadToolSurfaceOptions,
    view: &ThreadHostView,
    address: &str,
) -> Vec<ThreadAddressEntry> {
    to_thread_address_entries(&send_address_book(options, view, address, true))
}

pub async fn send_address_book_async(
    options: &ThreadToolSurfaceOptions,
    view: &ThreadHostView,
    address: &str,
    all_scope: bool,
) -> Vec<AddressEntry> {
    let options = options.clone();
    let view = view.clone();
    let address = address.to_owned();
    tokio::task::spawn_blocking(move || send_address_book(&options, &view, &address, all_scope))
        .await
        .expect("address book worker failed")
}

pub async fn send_book_entries_async(
    options: &ThreadToolSurfaceOptions,
    view: &ThreadHostView,
    address: &str,
) -> Vec<ThreadAddressEntry> {
    to_thread_address_entries(&send_address_book_async(options, view, address, true).await)
}

pub fn session_port(
    options: &ThreadToolSurfaceOptions,
    session: &ThreadHostSession,
) -> Arc<dyn ThreadSessionPort> {
    if let Some(socket) = session.socket.as_deref()
        && let Some(port) = options.host.endpoint(socket)
    {
        return port;
    }
    // Fall back to `options.host` as ThreadSessionPort
    Arc::new(HostPortAdapter {
        host: options.host.clone(),
    })
}

struct HostPortAdapter {
    host: Arc<dyn super::ports::ThreadHost>,
}

impl ThreadSessionPort for HostPortAdapter {
    fn get_messages(
        &self,
        session_id: &str,
    ) -> super::ports::BoxFuture<Result<Vec<super::super::reader::ThreadTranscriptEntry>, String>>
    {
        self.host.get_messages(session_id)
    }
    fn get_state(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<super::ports::SessionStateSnapshot, String>> {
        self.host.get_state(session_id, durable_session_id)
    }
    fn prompt(
        &self,
        session_id: &str,
        message: &str,
        options: Option<super::ports::PromptDeliveryOptions>,
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<Option<String>, String>> {
        self.host
            .prompt(session_id, message, options, durable_session_id)
    }
    fn interrupt(
        &self,
        session_id: &str,
        request: super::ports::InterruptRequest,
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<Value, String>> {
        self.host.interrupt(session_id, request, durable_session_id)
    }
    fn set_session_name(
        &self,
        session_id: &str,
        name: &str,
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<(), String>> {
        self.host
            .set_session_name(session_id, name, durable_session_id)
    }
    fn set_session_archived(
        &self,
        session_id: &str,
        archived: bool,
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<(), String>> {
        self.host
            .set_session_archived(session_id, archived, durable_session_id)
    }
    fn set_model(
        &self,
        session_id: &str,
        provider: &str,
        model_id: &str,
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<super::model_match::ModelChoice, String>> {
        self.host
            .set_model(session_id, provider, model_id, durable_session_id)
    }
    fn get_available_models(
        &self,
        session_id: &str,
    ) -> super::ports::BoxFuture<Result<Vec<super::model_match::ModelChoice>, String>> {
        self.host.get_available_models(session_id)
    }
    fn set_thinking_level(
        &self,
        session_id: &str,
        level: &str,
        scope: Option<&str>,
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<(), String>> {
        self.host
            .set_thinking_level(session_id, level, scope, durable_session_id)
    }
    fn get_available_thinking_levels(
        &self,
        session_id: &str,
    ) -> super::ports::BoxFuture<Result<Vec<String>, String>> {
        self.host.get_available_thinking_levels(session_id)
    }
    fn wake(
        &self,
        session_id: &str,
        delivery_ids: &[String],
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<super::super::adapter::GatewayWakeReply, String>> {
        self.host.wake(session_id, delivery_ids, durable_session_id)
    }
    fn release_session(
        &self,
        session_id: &str,
        request: super::super::adapter::ReleaseSessionRequest,
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<super::super::adapter::ReleaseSessionReply, String>> {
        self.host
            .release_session(session_id, request, durable_session_id)
    }
    fn close_session(
        &self,
        session_id: &str,
        durable_session_id: Option<&str>,
        options: super::ports::ThreadCloseOptions,
    ) -> super::ports::BoxFuture<Result<super::ports::ThreadCloseAnswer, String>> {
        self.host
            .close_session(session_id, durable_session_id, options)
    }
    fn watch_session(
        &self,
        session_id: &str,
        handlers: Arc<dyn super::ports::SessionWatchHandlers>,
        durable_session_id: Option<&str>,
    ) -> super::ports::BoxFuture<Result<super::ports::SessionWatchOutcome, String>> {
        self.host
            .watch_session(session_id, handlers, durable_session_id)
    }
    fn read_entry_page(
        &self,
        session_id: &str,
        max_bytes: Option<usize>,
        cursor: Option<&str>,
    ) -> super::ports::BoxFuture<Result<Option<super::super::paged_reader::EntryPageRead>, String>>
    {
        self.host.read_entry_page(session_id, max_bytes, cursor)
    }
}

pub fn routing_id(session: &ThreadHostSession) -> &str {
    &session.session_id
}

pub fn target_session<'a>(
    view: &'a ThreadHostView,
    durable_id: &str,
) -> Option<&'a ThreadHostSession> {
    view.sessions.iter().find(|session| {
        session
            .durable_session_id
            .as_deref()
            .unwrap_or(&session.session_id)
            == durable_id
    })
}
