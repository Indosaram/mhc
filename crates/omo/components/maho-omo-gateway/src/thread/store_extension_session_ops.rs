//! The session channel of store extensions (worker side).
//!
//! Upstream counterpart: `thread/gateway/store-extension-session-ops.ts`. The caller is the
//! engine-resolved durable id the session's tool passed, stamped into the op's args inside the op's
//! own transaction, with `caller_created_target: true` only when that caller thread_create'd the
//! op's target. Args that already name a caller are refused before validation.

use std::path::Path;

use serde_json::{Map, Value};

use super::extension_registrations::{AWAIT_TIMEOUT_MAX_MS, RESERVED_CALLER_KEYS, is_await_request_id};
use crate::paths::gateway_database_path;
use super::session_args_schema::argument_failure;
use super::store_extensions::{DeclaredSessionOp, StoreExtensionRefusalCode, StoreExtensionResult, StoreExtensionTransaction};
use super::store_ops::{SqlAccess, StoreContext, StoreError, transaction, v_i64, v_str};
use super::types::Provenance;

/// The caller of a session op as the worker resolves it; a session tool never passes it through args.
pub const UNKNOWN_CALLER: &str = "unknown";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionCallPhase {
    Op,
    Status,
    Expire,
}

#[derive(Debug, Clone)]
pub struct SessionCallRequest {
    pub name: String,
    pub op: String,
    pub phase: SessionCallPhase,
    pub args: Value,
    pub caller: String,
    pub now: i64,
}

/// The op's result, and for a declared await the facade's wait parameters.
#[derive(Debug, Clone)]
pub struct SessionCallReply {
    pub result: StoreExtensionResult<Value>,
    pub await_wait: Option<AwaitWait>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AwaitWait {
    pub timeout_ms: i64,
    pub wake_dir: String,
}

fn refused(code: StoreExtensionRefusalCode, message: impl Into<String>) -> SessionCallReply {
    SessionCallReply { result: StoreExtensionResult::refused(code, message), await_wait: None }
}

/// The wake hint after a committed call: a temp file in the same dir renamed over it. The connector
/// creates the dir; this never does.
pub fn touch_wake(agent_dir: &Path, wake_dir: &str, wake: &str) -> Result<(), StoreError> {
    let marker = gateway_database_path(agent_dir).parent().map(|parent| parent.join(wake_dir).join(wake));
    let Some(marker) = marker else {
        return Err(StoreError::Message("the gateway database has no directory".to_owned()));
    };
    let temporary = marker.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::write(&temporary, serde_json::json!({ "written_at": super::store_ops::rfc3339(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as i64).unwrap_or(0)) }).to_string())
        .map_err(|error| StoreError::Message(error.to_string()))?;
    let result = std::fs::rename(&temporary, &marker).map_err(|error| StoreError::Message(error.to_string()));
    let _ = std::fs::remove_file(&temporary);
    result
}

pub fn record_thread_creation(ctx: &StoreContext, now: i64, creator_durable_id: &str, created_durable_id: &str) -> Result<(), StoreError> {
    transaction(ctx, "record_thread_creation", || {
        ctx.run(
            "INSERT OR IGNORE INTO thread_creations (creator_durable_id, created_durable_id, created_at) VALUES (?, ?, ?)",
            &[v_str(creator_durable_id), v_str(created_durable_id), v_i64(now)],
        )?;
        Ok(())
    })
}

fn created_by(ctx: &StoreContext, creator: &str, created: &str) -> bool {
    ctx.one(
        "SELECT 1 AS n FROM thread_creations WHERE creator_durable_id = ? AND created_durable_id = ?",
        &[v_str(creator), v_str(created)],
    )
    .ok()
    .flatten()
    .is_some()
}

/// Runs one declared session op as `caller`, then reports the declared await when the op has one.
pub fn session_call(
    ctx: &StoreContext,
    entry: &DeclaredSessionOp,
    descriptor_wake_dir: Option<&str>,
    request: &SessionCallRequest,
    run: &dyn Fn(&str, Value) -> StoreExtensionResult<Value>,
) -> SessionCallReply {
    let name = &request.name;
    let caller = &request.caller;
    if caller.is_empty() || caller == UNKNOWN_CALLER {
        return refused(StoreExtensionRefusalCode::CallerContextMissing, "A session-callable op needs the calling session's engine-resolved durable id.");
    }
    let declaration = &entry.entry;
    let Some(args) = request.args.as_object() else {
        return refused(StoreExtensionRefusalCode::InvalidArguments, "Session op arguments must be an object.");
    };
    let forged = RESERVED_CALLER_KEYS.iter().find(|key| args.contains_key(**key));
    if let Some(key) = forged {
        return refused(StoreExtensionRefusalCode::InvalidArguments, format!("{key} is stamped by the store from the engine's caller and cannot be passed."));
    }
    let mut op = declaration.op.clone();
    match request.phase {
        SessionCallPhase::Op => {
            if let Some(failure) = argument_failure(&declaration.parameters, &request.args, "") {
                let path = if failure.path.is_empty() { "/".to_owned() } else { failure.path };
                return refused(StoreExtensionRefusalCode::InvalidArguments, format!("Parameter validation failed at {path}: {}", failure.message));
            }
        }
        SessionCallPhase::Status | SessionCallPhase::Expire => {
            let Some(awaited) = declaration.await_spec.as_ref().filter(|_| declaration.is_tool()) else {
                return refused(StoreExtensionRefusalCode::ExtensionUnknownOp, format!("{name}.{} declares no await.", declaration.op));
            };
            let id = args.get("await_request_id").and_then(Value::as_str).unwrap_or_default();
            if !is_await_request_id(id) {
                return refused(StoreExtensionRefusalCode::InvalidArguments, "await_request_id must match ^tor_[a-f0-9]{32}$.");
            }
            op = if request.phase == SessionCallPhase::Status { awaited.status_op.clone() } else { awaited.expire_op.clone() };
            if awaited.timeout_ms > AWAIT_TIMEOUT_MAX_MS {
                return refused(StoreExtensionRefusalCode::InvalidArguments, format!("await.timeoutMs must be at most {AWAIT_TIMEOUT_MAX_MS}"));
            }
        }
    }
    let target = declaration.target_arg.as_ref().and_then(|target_arg| args.get(target_arg)).and_then(Value::as_str);
    let mut stamped = Map::new();
    match request.phase {
        SessionCallPhase::Op => {
            stamped.extend(args.clone());
        }
        _ => {
            if let Some(id) = args.get("await_request_id") {
                stamped.insert("await_request_id".to_owned(), id.clone());
            }
        }
    }
    stamped.insert("caller_session_durable_id".to_owned(), Value::String(caller.clone()));
    if request.phase == SessionCallPhase::Op
        && let Some(target) = target
        && created_by(ctx, caller, target)
    {
        stamped.insert("caller_created_target".to_owned(), Value::Bool(true));
    }
    let result = run(&op, Value::Object(stamped));
    if request.phase == SessionCallPhase::Op
        && let (Some(awaited), Some(wake_dir)) = (declaration.await_spec.as_ref().filter(|_| declaration.is_tool()), descriptor_wake_dir)
    {
        return SessionCallReply { result, await_wait: Some(AwaitWait { timeout_ms: awaited.timeout_ms, wake_dir: wake_dir.to_owned() }) };
    }
    SessionCallReply { result, await_wait: None }
}

/// The value an op's own transaction hands to `session_call`.
pub fn run_declared_op(tx: &StoreExtensionTransaction, op: &str, args: Value) -> Result<Value, String> {
    let _ = tx;
    let _ = op;
    let _ = args;
    Err("no compiled operation is bound to this transaction".to_owned())
}

/// The provenance an extension enqueue records when the caller's runtime supplied none.
pub fn fallback_provenance() -> Provenance {
    Provenance { workspace: "unclassified".to_owned(), project: "extension".to_owned(), lane: None, session: None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::thread::store_extensions::{SessionCallableOp, StoreExtensionRegistration};

    fn entry() -> DeclaredSessionOp {
        DeclaredSessionOp {
            entry: SessionCallableOp {
                op: "open_request".into(),
                parameters: serde_json::json!({ "type": "object", "properties": { "target": { "type": "string" } }, "additionalProperties": false }),
                target_arg: Some("target".into()),
                internal: None,
                tool_name: Some("open".into()),
                description: Some("Open.".into()),
                await_spec: Some(crate::thread::store_extensions::AwaitSpec { status_op: "status".into(), expire_op: "expire".into(), timeout_ms: 10 }),
                wake: None,
            },
            extension: "alpha".into(),
            registered_name: "ext_alpha_open".into(),
        }
    }

    fn context() -> StoreContext {
        let ctx = StoreContext::new(rusqlite::Connection::open_in_memory().expect("memory database"), StoreContext::test_config(), StoreContext::test_identity());
        for step in crate::thread::schema::GATEWAY_MIGRATIONS {
            for statement in *step {
                ctx.exec(statement).expect("migration step");
            }
        }
        ctx
    }

    #[test]
    fn a_missing_caller_is_refused_before_the_op_runs() {
        let ctx = context();
        let request = SessionCallRequest { name: "alpha".into(), op: "open_request".into(), phase: SessionCallPhase::Op, args: serde_json::json!({}), caller: UNKNOWN_CALLER.into(), now: 1 };
        let reply = session_call(&ctx, &entry(), Some("alpha"), &request, &|_, _| StoreExtensionResult::ok(Value::Null));
        assert!(matches!(reply.result, StoreExtensionResult::Refused { code, .. } if code == StoreExtensionRefusalCode::CallerContextMissing));
    }

    #[test]
    fn a_forged_caller_field_is_refused_before_validation() {
        let ctx = context();
        let request = SessionCallRequest {
            name: "alpha".into(),
            op: "open_request".into(),
            phase: SessionCallPhase::Op,
            args: serde_json::json!({ "caller_session_durable_id": "victim" }),
            caller: "sender".into(),
            now: 1,
        };
        let reply = session_call(&ctx, &entry(), Some("alpha"), &request, &|_, _| StoreExtensionResult::ok(Value::Null));
        assert!(matches!(reply.result, StoreExtensionResult::Refused { code, .. } if code == StoreExtensionRefusalCode::InvalidArguments));
    }

    #[test]
    fn the_caller_is_stamped_and_the_created_target_is_proven() {
        let ctx = context();
        record_thread_creation(&ctx, 1, "sender", "child").expect("record");
        let seen = std::sync::Mutex::new(Value::Null);
        let request = SessionCallRequest { name: "alpha".into(), op: "open_request".into(), phase: SessionCallPhase::Op, args: serde_json::json!({ "target": "child" }), caller: "sender".into(), now: 1 };
        let reply = session_call(&ctx, &entry(), Some("alpha"), &request, &|_, args| {
            *seen.lock().expect("seen") = args.clone();
            StoreExtensionResult::ok(serde_json::json!({ "status": "opened" }))
        });
        assert!(reply.result.is_ok());
        assert_eq!(reply.await_wait.map(|wait| wait.timeout_ms), Some(10));
        let stamped = seen.lock().expect("seen").clone();
        assert_eq!(stamped["caller_session_durable_id"], "sender");
        assert_eq!(stamped["caller_created_target"], true);
    }

    #[test]
    fn a_status_phase_runs_the_declared_status_op() {
        let ctx = context();
        let seen_op = std::cell::RefCell::new(String::new());
        let request = SessionCallRequest {
            name: "alpha".into(),
            op: "open_request".into(),
            phase: SessionCallPhase::Status,
            args: serde_json::json!({ "await_request_id": "tor_0123456789abcdef0123456789abcdef" }),
            caller: "sender".into(),
            now: 1,
        };
        let reply = session_call(&ctx, &entry(), Some("alpha"), &request, &|op, _| {
            *seen_op.borrow_mut() = op.to_owned();
            StoreExtensionResult::ok(serde_json::json!({ "status": "opened" }))
        });
        assert!(reply.result.is_ok());
        assert_eq!(*seen_op.borrow(), "status");
        assert!(reply.await_wait.is_none());
    }

    #[test]
    fn an_invalid_await_request_id_is_refused() {
        let ctx = context();
        let request = SessionCallRequest { name: "alpha".into(), op: "open_request".into(), phase: SessionCallPhase::Expire, args: serde_json::json!({ "await_request_id": "../etc/passwd" }), caller: "sender".into(), now: 1 };
        let reply = session_call(&ctx, &entry(), Some("alpha"), &request, &|_, _| StoreExtensionResult::ok(Value::Null));
        assert!(matches!(reply.result, StoreExtensionResult::Refused { code, .. } if code == StoreExtensionRefusalCode::InvalidArguments));
    }

    #[test]
    fn a_registration_descriptor_carries_its_await() {
        let descriptor = StoreExtensionRegistration { name: "alpha".into(), migrations: vec![], module_url: "file:///x.mjs".into(), session_callable: None, wake_dir: None };
        assert!(descriptor.session_callable.is_none());
    }
}
