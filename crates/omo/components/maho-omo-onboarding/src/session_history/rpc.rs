//! Session-history RPC methods (upstream `session-history-rpc.ts`).
//!
//! `omo.onboarding.projects` answers the engine's project scan; `omo.onboarding.sessions` answers
//! `{scanId}` and then pushes one `omo.onboarding.sessions.event` per outcome, ending with done or failed.

use std::sync::Arc;

use maho_ext_api::{EventBus, ExtensionApi, ExtensionFailure};
use serde_json::{Map, Value, json};

use super::types::{
    AgentHome, AgentSessionSource, CompletedTranscript, ImportedProject, RecentSessionOutcome,
    RecentSessionsQuery, SessionHistoryOptions,
};
use super::{SessionHistoryScanner, create_session_history_scanner};

pub const OMO_SESSION_PROJECTS_RPC_METHOD: &str = "omo.onboarding.projects";
pub const OMO_SESSION_THREADS_RPC_METHOD: &str = "omo.onboarding.sessions";
pub const OMO_SESSION_THREADS_RPC_EVENT: &str = "omo.onboarding.sessions.event";

pub type CreateScanner = Arc<dyn Fn(SessionHistoryOptions) -> SessionHistoryScanner + Send + Sync>;
pub type NewScanId = Arc<dyn Fn() -> String + Send + Sync>;

#[derive(Clone, Default)]
pub struct SessionHistoryRpcOptions {
    /// Test seam; production builds the scanner from the module defaults.
    pub create_scanner: Option<CreateScanner>,
    pub new_scan_id: Option<NewScanId>,
}

fn unavailable(reason: &str) -> Value {
    json!({"kind": "unavailable", "reason": reason})
}

struct OwnDirectories {
    excluded_directories: Vec<String>,
    managed_worktree_directories: Vec<String>,
}

pub struct ProjectsRequest {
    homes: Vec<AgentHome>,
    imported_projects: Vec<ImportedProject>,
    own: OwnDirectories,
}

pub struct SessionsRequest {
    workspace_root: String,
    homes: Vec<AgentHome>,
    completed: Vec<CompletedTranscript>,
    own: OwnDirectories,
}

fn parse_list<T>(value: Option<&Value>, parse: impl Fn(&Value) -> Option<T>) -> Option<Vec<T>> {
    value?.as_array()?.iter().map(parse).collect()
}

fn parse_homes(value: Option<&Value>) -> Option<Vec<AgentHome>> {
    parse_list(value, |entry| {
        let entry = entry.as_object()?;
        Some(AgentHome {
            source: AgentSessionSource::parse(entry.get("source")?.as_str()?)?,
            home_path: entry.get("homePath")?.as_str()?.to_owned(),
        })
    })
}

fn number_or_null(value: Option<&Value>) -> Option<Option<f64>> {
    match value? {
        Value::Null => Some(None),
        Value::Number(n) => Some(n.as_f64()),
        _ => None,
    }
}

fn parse_completed(entry: &Value) -> Option<CompletedTranscript> {
    let entry = entry.as_object()?;
    let to_u64 = |value: f64| (value >= 0.0 && value.fract() == 0.0).then_some(value as u64);
    Some(CompletedTranscript {
        provider: AgentSessionSource::parse(entry.get("provider")?.as_str()?)?,
        provider_session_id: entry.get("providerSessionId")?.as_str()?.to_owned(),
        file_path: entry.get("filePath")?.as_str()?.to_owned(),
        size: to_u64(entry.get("size")?.as_f64()?)?,
        mtime_ms: number_or_null(entry.get("mtimeMs"))?.map(|v| v as i64),
        device: to_u64(entry.get("device")?.as_f64()?)?,
        inode: number_or_null(entry.get("inode"))?.and_then(to_u64),
        birthtime_ms: number_or_null(entry.get("birthtimeMs"))?.map(|v| v as i64),
    })
}

fn parse_own_directories(data: &Map<String, Value>) -> Option<OwnDirectories> {
    let strings = |key: &str| parse_list(data.get(key), |entry| entry.as_str().map(str::to_owned));
    Some(OwnDirectories {
        excluded_directories: strings("excludedDirectories")?,
        managed_worktree_directories: strings("managedWorktreeDirectories")?,
    })
}

const HOMES_ERROR: &str = "homes must list {source, homePath} with a claudeAgent or codex source.";
const OWN_ERROR: &str = "excludedDirectories and managedWorktreeDirectories must be string lists.";

pub fn parse_projects_request(data: &Value) -> Result<ProjectsRequest, Value> {
    let Some(data) = data.as_object() else {
        return Err(unavailable("Request must be an object."));
    };
    let homes = parse_homes(data.get("homes"));
    let imported_projects = parse_list(data.get("importedProjects"), |entry| {
        let entry = entry.as_object()?;
        Some(ImportedProject {
            id: entry.get("id")?.as_str()?.to_owned(),
            workspace_root: entry.get("workspaceRoot")?.as_str()?.to_owned(),
        })
    });
    let own = parse_own_directories(data);
    let homes = homes.ok_or_else(|| unavailable(HOMES_ERROR))?;
    let imported_projects = imported_projects
        .ok_or_else(|| unavailable("importedProjects must list {id, workspaceRoot}."))?;
    let own = own.ok_or_else(|| unavailable(OWN_ERROR))?;
    Ok(ProjectsRequest {
        homes,
        imported_projects,
        own,
    })
}

pub fn parse_sessions_request(data: &Value) -> Result<SessionsRequest, Value> {
    let Some(data) = data.as_object() else {
        return Err(unavailable("Request must be an object."));
    };
    let homes = parse_homes(data.get("homes"));
    let completed = parse_list(data.get("completed"), parse_completed);
    let own = parse_own_directories(data);
    let workspace_root = match data.get("workspaceRoot").and_then(Value::as_str) {
        Some(root) if !root.is_empty() => root.to_owned(),
        _ => return Err(unavailable("workspaceRoot must be a non-empty string.")),
    };
    let homes = homes.ok_or_else(|| unavailable(HOMES_ERROR))?;
    let completed =
        completed.ok_or_else(|| unavailable("completed must list transcript identities."))?;
    let own = own.ok_or_else(|| unavailable(OWN_ERROR))?;
    Ok(SessionsRequest {
        workspace_root,
        homes,
        completed,
        own,
    })
}

/// Importable outcomes are wired as `{_tag, thread, source}`; the others as they are.
pub fn wire_outcome(outcome: &RecentSessionOutcome) -> Value {
    match outcome {
        RecentSessionOutcome::Importable { thread, source, .. } => {
            json!({"_tag": "Importable", "thread": thread.to_json(), "source": source.to_json()})
        }
        RecentSessionOutcome::AlreadyImported { source } => {
            json!({"_tag": "AlreadyImported", "source": source.to_json()})
        }
        RecentSessionOutcome::Duplicate { source } => {
            json!({"_tag": "Duplicate", "source": source.to_json()})
        }
        RecentSessionOutcome::Skipped => json!({"_tag": "Skipped"}),
    }
}

fn emit_event(events: &EventBus, scan_id: &str, event: Value) {
    let mut data = Map::new();
    data.insert("scanId".into(), json!(scan_id));
    if let Value::Object(fields) = event {
        data.extend(fields);
    }
    events.emit(
        "senpi:extension-rpc-event",
        &json!({"name": OMO_SESSION_THREADS_RPC_EVENT, "data": Value::Object(data)}),
    );
}

/// Runs a recent-sessions scan and reports each step through `emit`.
pub fn run_sessions_scan(
    scanner: &SessionHistoryScanner,
    request: &SessionsRequest,
    emit: &mut dyn FnMut(Value),
) {
    let run = scanner.recent_sessions(RecentSessionsQuery {
        workspace_root: Some(request.workspace_root.clone()),
        recent_window_ms: None,
        completed: Some(request.completed.clone()),
    });
    for outcome in run {
        emit(json!({"kind": "outcome", "outcome": wire_outcome(&outcome)}));
    }
    emit(json!({"kind": "done"}));
}

fn scanner_for(
    create: &Option<CreateScanner>,
    options: SessionHistoryOptions,
) -> SessionHistoryScanner {
    match create {
        Some(create) => create(options),
        None => create_session_history_scanner(options),
    }
}

/// The desktop's thread import reads the engine's session history through these two methods, so discovery and
/// transcript parsing exist once, here. Both are read-only filesystem work: no model, no session.
pub fn register_session_history_rpc(
    api: &mut ExtensionApi,
    options: SessionHistoryRpcOptions,
) -> Result<(), ExtensionFailure> {
    let create = options.create_scanner.clone();
    api.rpc_handle(
        OMO_SESSION_PROJECTS_RPC_METHOD,
        Arc::new(move |data| {
            let create = create.clone();
            Box::pin(async move {
                let request = match parse_projects_request(&data) {
                    Ok(request) => request,
                    Err(reply) => return Ok(reply),
                };
                let scanner = scanner_for(
                    &create,
                    SessionHistoryOptions {
                        homes: Some(request.homes),
                        imported_projects: Some(request.imported_projects),
                        excluded_directories: Some(request.own.excluded_directories),
                        managed_worktree_directories: Some(
                            request.own.managed_worktree_directories,
                        ),
                        ..Default::default()
                    },
                );
                Ok(scanner.scan_projects().to_json())
            })
        }),
    )?;

    let create = options.create_scanner.clone();
    let new_scan_id: NewScanId = options
        .new_scan_id
        .clone()
        .unwrap_or_else(|| Arc::new(|| uuid::Uuid::new_v4().to_string()));
    let events = api.events.clone();
    api.rpc_handle(
        OMO_SESSION_THREADS_RPC_METHOD,
        Arc::new(move |data| {
            let create = create.clone();
            let new_scan_id = Arc::clone(&new_scan_id);
            let events = events.clone();
            Box::pin(async move {
                let request = match parse_sessions_request(&data) {
                    Ok(request) => request,
                    Err(reply) => return Ok(reply),
                };
                let scan_id = new_scan_id();
                let reply = json!({"scanId": scan_id});
                // The scan runs on its own thread so the reply is not held behind it.
                std::thread::spawn(move || {
                    let scanner = scanner_for(
                        &create,
                        SessionHistoryOptions {
                            homes: Some(request.homes.clone()),
                            excluded_directories: Some(request.own.excluded_directories.clone()),
                            managed_worktree_directories: Some(
                                request.own.managed_worktree_directories.clone(),
                            ),
                            ..Default::default()
                        },
                    );
                    run_sessions_scan(&scanner, &request, &mut |event| {
                        emit_event(&events, &scan_id, event)
                    });
                });
                Ok(reply)
            })
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reason(result: Result<impl Sized, Value>) -> Option<String> {
        result
            .err()
            .and_then(|v| v.get("reason").and_then(Value::as_str).map(str::to_owned))
    }

    #[test]
    fn projects_request_validation_messages() {
        assert_eq!(
            reason(parse_projects_request(&json!("x"))).as_deref(),
            Some("Request must be an object.")
        );
        let ok = json!({"homes": [], "importedProjects": [], "excludedDirectories": [], "managedWorktreeDirectories": []});
        assert!(parse_projects_request(&ok).is_ok());
        let bad_homes = json!({"homes": [{"source": "x", "homePath": "/h"}], "importedProjects": [], "excludedDirectories": [], "managedWorktreeDirectories": []});
        assert_eq!(
            reason(parse_projects_request(&bad_homes)).as_deref(),
            Some("homes must list {source, homePath} with a claudeAgent or codex source.")
        );
        let bad_imported = json!({"homes": [], "importedProjects": [{"id": 1}], "excludedDirectories": [], "managedWorktreeDirectories": []});
        assert_eq!(
            reason(parse_projects_request(&bad_imported)).as_deref(),
            Some("importedProjects must list {id, workspaceRoot}.")
        );
        let bad_dirs = json!({"homes": [], "importedProjects": [], "excludedDirectories": [1], "managedWorktreeDirectories": []});
        assert_eq!(
            reason(parse_projects_request(&bad_dirs)).as_deref(),
            Some("excludedDirectories and managedWorktreeDirectories must be string lists.")
        );
    }

    #[test]
    fn sessions_request_validation_messages_and_completed_identity() {
        let base = |root: Value, completed: Value| json!({"workspaceRoot": root, "homes": [], "completed": completed, "excludedDirectories": [], "managedWorktreeDirectories": []});
        assert_eq!(
            reason(parse_sessions_request(&base(json!(""), json!([])))).as_deref(),
            Some("workspaceRoot must be a non-empty string.")
        );
        assert_eq!(
            reason(parse_sessions_request(&base(
                json!("/p"),
                json!([{"provider": "codex"}])
            )))
            .as_deref(),
            Some("completed must list transcript identities.")
        );
        let entry = json!({"provider": "codex", "providerSessionId": "s", "filePath": "/f", "size": 3, "mtimeMs": null, "device": 1, "inode": 7, "birthtimeMs": null});
        let parsed = parse_sessions_request(&base(json!("/p"), json!([entry])));
        assert!(
            parsed
                .as_ref()
                .is_ok_and(|r| r.completed.len() == 1 && r.completed[0].inode == Some(7))
        );
    }

    #[test]
    fn importable_outcomes_drop_cwd_and_digest_on_the_wire() {
        assert_eq!(
            wire_outcome(&RecentSessionOutcome::Skipped),
            json!({"_tag": "Skipped"})
        );
    }
}
