//! Port of senpi `packages/coding-agent/src/modes/rpc/worker-admission-command.ts`
//! (and admission reply AsyncLocalStorage scope from `compact-admission-reply.ts`).

use crate::session_worker_owners::{ExtensionOwner, ExtensionOwnerWire, IssuedWorkerOwners};
use futures::future::BoxFuture;
use maho_core::engine_socket_flag::compact_updates_enabled;
use std::future::Future;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionReplyScope {
    pub id: Option<String>,
    pub admission_id: Option<String>,
    pub peer: Option<String>,
}

tokio::task_local! {
    static ADMISSION_REQUEST: AdmissionReplyScope;
}

/// Bounded reply identity, not the command body or a retained request index, crosses an isolate.
pub fn capture_admission_reply() -> Option<AdmissionReplyScope> {
    ADMISSION_REQUEST.try_with(Clone::clone).ok()
}

pub async fn with_admission_reply<T, F>(scope: Option<AdmissionReplyScope>, action: F) -> T
where
    F: Future<Output = T>,
{
    match scope {
        None => action.await,
        Some(scope) => ADMISSION_REQUEST.scope(scope, action).await,
    }
}

pub async fn with_admission_request<T, F>(
    id: Option<String>,
    admission_id: Option<String>,
    peer: Option<String>,
    action: F,
) -> T
where
    F: Future<Output = T>,
{
    if !compact_updates_enabled() {
        return action.await;
    }
    ADMISSION_REQUEST
        .scope(
            AdmissionReplyScope {
                id,
                admission_id,
                peer,
            },
            action,
        )
        .await
}

pub fn reply_admission<'a>(
    id: &str,
    peer: Option<&str>,
    routes: &'a [String],
) -> Option<Option<&'a str>> {
    let request = ADMISSION_REQUEST.try_with(Clone::clone).ok()?;
    if request.id.as_deref() != Some(id) || request.peer.as_deref() != peer {
        return Some(None);
    }
    request.admission_id.as_deref().map(|admission_id| {
        routes
            .iter()
            .find(|r| r.as_str() == admission_id)
            .map(String::as_str)
    })
}

pub trait WorkerCommandContext: Send + Sync {
    fn with_connection(
        &self,
        connection: &str,
        run: BoxFuture<'static, ()>,
    ) -> BoxFuture<'static, ()>;
    fn run_issued(
        &self,
        connection: &str,
        owner: ExtensionOwner,
        run: BoxFuture<'static, ()>,
    ) -> BoxFuture<'static, ()>;
}

pub async fn worker_admission_command<F>(
    admission_reply: Option<AdmissionReplyScope>,
    connection: Option<&str>,
    owner: Option<ExtensionOwnerWire>,
    context: &dyn WorkerCommandContext,
    owners: &Mutex<IssuedWorkerOwners>,
    run: F,
) where
    F: Future<Output = ()> + Send + 'static,
{
    with_admission_reply(admission_reply, async move {
        let boxed: BoxFuture<'static, ()> = Box::pin(run);
        match (connection, owner) {
            (None, _) => boxed.await,
            (Some(conn), None) => context.with_connection(conn, boxed).await,
            (Some(conn), Some(wire)) => {
                let adopted = owners
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .adopt(wire);
                context.run_issued(conn, adopted, boxed).await;
            }
        }
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::AtomicI32;

    struct TestContext {
        log: Arc<Mutex<Vec<String>>>,
    }

    impl WorkerCommandContext for TestContext {
        fn with_connection(
            &self,
            connection: &str,
            run: BoxFuture<'static, ()>,
        ) -> BoxFuture<'static, ()> {
            let log = self.log.clone();
            let conn = connection.to_owned();
            Box::pin(async move {
                log.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("conn:{conn}"));
                run.await;
            })
        }

        fn run_issued(
            &self,
            connection: &str,
            owner: ExtensionOwner,
            run: BoxFuture<'static, ()>,
        ) -> BoxFuture<'static, ()> {
            let log = self.log.clone();
            let conn = connection.to_owned();
            let oid = owner.wire.owner_id.clone();
            Box::pin(async move {
                log.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(format!("issued:{conn}:{oid}"));
                run.await;
            })
        }
    }

    #[tokio::test]
    async fn worker_admission_command_preserves_scope_and_selects_owner_path() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let ctx = TestContext { log: log.clone() };
        let owners = Mutex::new(IssuedWorkerOwners::default());
        let scope = AdmissionReplyScope {
            id: Some("cmd-1".into()),
            admission_id: Some("adm-1".into()),
            peer: Some("p1".into()),
        };
        let wire = ExtensionOwnerWire {
            owner_id: "o1".into(),
            binding_generation: "g1".into(),
            retirement: Arc::new(AtomicI32::new(0)),
        };
        let log2 = log.clone();
        worker_admission_command(
            Some(scope),
            Some("p1"),
            Some(wire),
            &ctx,
            &owners,
            async move {
                let cap = capture_admission_reply().expect("scope");
                assert_eq!(cap.admission_id.as_deref(), Some("adm-1"));
                log2.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push("ran".into());
            },
        )
        .await;
        assert_eq!(
            *log.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            vec!["issued:p1:o1", "ran"]
        );
    }
}
