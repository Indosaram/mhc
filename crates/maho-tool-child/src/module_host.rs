//! The host a registered child module's export receives as its second argument (senpi `tool-child/module-host.ts`;
//! senpi#3566 todo 8). It is the same object on both runtimes: only `send` differs - the tool child's worker posts each
//! call to the reverse bridge, the in-process path hands it straight to the session's host-call handler - so a module
//! behaves the same wherever the session places it.
use std::future::Future;
use std::sync::Arc;

use futures::future::BoxFuture;
use maho_ext_api::AbortSignal;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::protocol::HostCallKind;

type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// `ModuleHostSend`: one host call `(kind, name, args)`; `args` is `Value::Null` when upstream passes `undefined`.
pub type ModuleHostSend = Arc<
    dyn Fn(HostCallKind, Option<String>, Value) -> BoxFuture<'static, Result<Value, BoxError>>
        + Send
        + Sync,
>;

/// `operation` of an `authorize` request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthorizeOperation {
    Read,
    Enumerate,
    Write,
}

/// `{ operation, path, toolName }`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthorizeRequest {
    pub operation: AuthorizeOperation,
    pub path: String,
    pub tool_name: String,
}

/// `{ allow: true } | { allow: false; reason: string }`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthorizeDecision {
    Allow,
    Deny { reason: String },
}

impl AuthorizeDecision {
    fn from_value(value: &Value) -> Result<Self, BoxError> {
        let allow = value.get("allow").and_then(Value::as_bool);
        match allow {
            Some(true) => Ok(Self::Allow),
            Some(false) => Ok(Self::Deny {
                reason: value
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            None => Err("authorize reply has no `allow`".into()),
        }
    }
}

#[derive(Clone)]
pub struct ModuleHost {
    /// Aborts when the job is cancelled.
    pub signal: AbortSignal,
    send: ModuleHostSend,
}

impl ModuleHost {
    /// Runs a session tool on the server (hooks once), like `ctx.executeTool`.
    pub async fn call(&self, name: &str, args: Value) -> Result<Value, BoxError> {
        (self.send)(HostCallKind::Tool, Some(name.to_string()), args).await
    }

    /// A model completion through the session's runtime.
    pub async fn complete(&self, args: Value) -> Result<Value, BoxError> {
        (self.send)(HostCallKind::Completion, None, args).await
    }

    /// A reserved tool, run server-side.
    pub async fn reserved(&self, name: &str, args: Value) -> Result<Value, BoxError> {
        (self.send)(HostCallKind::Reserved, Some(name.to_string()), args).await
    }

    /// The tools callable through `call`.
    pub async fn list_tools(&self) -> Result<Value, BoxError> {
        (self.send)(HostCallKind::ListTools, None, Value::Null).await
    }

    /// The session's filesystem policy decision for one access (never cached; ask before each). The server resolves
    /// `path` against the session cwd and canonicalizes it the way the built-in file tools do.
    pub async fn authorize(
        &self,
        request: AuthorizeRequest,
    ) -> Result<AuthorizeDecision, BoxError> {
        let reply = (self.send)(
            HostCallKind::Authorize,
            None,
            serde_json::to_value(request)?,
        )
        .await?;
        AuthorizeDecision::from_value(&reply)
    }

    /// Runs `body` holding the file's slot in the session's mutation queue - the same one its own file tools use - and
    /// releases it once `body` settles. A cancel does not release it early: the slot passes on only after `body` is done.
    /// Inside `body`, never `call` a session tool that edits or writes the same file: it waits for this very slot and
    /// the job deadlocks until its deadline.
    pub async fn with_file_lock<T, F, Fut>(&self, path: &str, body: F) -> Result<T, BoxError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<T, BoxError>>,
    {
        let granted = (self.send)(HostCallKind::LockFile, None, json!({ "path": path })).await?;
        let lock_id = granted
            .get("lockId")
            .and_then(Value::as_str)
            .ok_or_else(|| BoxError::from("lock_file reply has no `lockId`"))?
            .to_string();
        let outcome = body().await;
        // `finally { await send("unlock_file", ...) }`: a failing unlock replaces the body's outcome.
        (self.send)(HostCallKind::UnlockFile, None, json!({ "lockId": lock_id })).await?;
        outcome
    }
}

pub fn create_module_host(signal: AbortSignal, send: ModuleHostSend) -> ModuleHost {
    ModuleHost { signal, send }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::FutureExt;
    use std::sync::Mutex;

    type Calls = Arc<Mutex<Vec<(HostCallKind, Option<String>, Value)>>>;

    fn host(
        reply: impl Fn(HostCallKind) -> Result<Value, String> + Send + Sync + 'static,
    ) -> (ModuleHost, Calls) {
        let calls: Calls = Arc::default();
        let log = Arc::clone(&calls);
        let send: ModuleHostSend = Arc::new(move |kind, name, args| {
            if let Ok(mut log) = log.lock() {
                log.push((kind, name, args));
            }
            let outcome = reply(kind).map_err(BoxError::from);
            async move { outcome }.boxed()
        });
        (create_module_host(AbortSignal::default(), send), calls)
    }

    #[tokio::test]
    async fn each_method_sends_its_host_call_kind() {
        let (host, calls) = host(|_| Ok(json!("ok")));
        host.call("t", json!(1)).await.expect("call");
        host.complete(json!(2)).await.expect("complete");
        host.reserved("r", json!(3)).await.expect("reserved");
        host.list_tools().await.expect("list");
        let seen = calls.lock().expect("lock").clone();
        assert_eq!(
            seen,
            vec![
                (HostCallKind::Tool, Some("t".to_string()), json!(1)),
                (HostCallKind::Completion, None, json!(2)),
                (HostCallKind::Reserved, Some("r".to_string()), json!(3)),
                (HostCallKind::ListTools, None, Value::Null),
            ]
        );
    }

    #[tokio::test]
    async fn authorize_sends_the_request_and_parses_the_decision() {
        let (host, calls) = host(|_| Ok(json!({ "allow": false, "reason": "nope" })));
        let decision = host
            .authorize(AuthorizeRequest {
                operation: AuthorizeOperation::Write,
                path: "/p".to_string(),
                tool_name: "write".to_string(),
            })
            .await
            .expect("authorize");
        assert_eq!(
            decision,
            AuthorizeDecision::Deny {
                reason: "nope".to_string()
            }
        );
        let seen = calls.lock().expect("lock").clone();
        assert_eq!(
            seen[0].2,
            json!({ "operation": "write", "path": "/p", "toolName": "write" })
        );
    }

    #[tokio::test]
    async fn with_file_lock_unlocks_after_the_body_even_when_it_fails() {
        let (host, calls) = host(|kind| match kind {
            HostCallKind::LockFile => Ok(json!({ "lockId": "L1" })),
            _ => Ok(Value::Null),
        });
        let outcome: Result<(), BoxError> = host
            .with_file_lock("/f", || async { Err("boom".into()) })
            .await;
        assert_eq!(outcome.expect_err("body failed").to_string(), "boom");
        let seen = calls.lock().expect("lock").clone();
        assert_eq!(
            seen[0],
            (HostCallKind::LockFile, None, json!({ "path": "/f" }))
        );
        assert_eq!(
            seen[1],
            (HostCallKind::UnlockFile, None, json!({ "lockId": "L1" }))
        );
    }
}
