use std::{path::PathBuf, sync::{Arc, Mutex}};
use serde_json::Value;

tokio::task_local! {
    pub(super) static CURRENT: Arc<Invocation>;
}

pub(super) struct Invocation {
    pub(super) session_identity: usize,
    pub(super) generation: u64,
    id: String,
    name: String,
    input: Value,
    cwd: PathBuf,
    state: Mutex<State>,
}

enum State {
    Preflight(Option<PathBuf>),
    Executable(Option<PathBuf>),
    Retired,
}

pub(super) struct Retirement(pub(super) Arc<Invocation>);
impl Drop for Retirement {
    fn drop(&mut self) { *self.0.state.lock().expect("invocation state") = State::Retired; }
}

impl Invocation {
    pub(super) fn new(session_identity: usize, generation: u64, id: String, name: String, input: Value, cwd: PathBuf) -> Arc<Self> {
        Arc::new(Self { session_identity, generation, id, name, input, cwd, state: Mutex::new(State::Preflight(None)) })
    }

    fn matches(&self, id: &str, input: &Value, cwd: &std::path::Path) -> Result<(), String> {
        if self.id != id || self.input != *input || self.cwd != cwd {
            *self.state.lock().expect("invocation state") = State::Retired;
            return Err("Monitor invocation identity changed".into());
        }
        Ok(())
    }

    pub(super) fn attach(&self, id: &str, input: &Value, cwd: &std::path::Path, parent: PathBuf) -> Result<(), String> {
        self.matches(id, input, cwd)?;
        let mut state = self.state.lock().expect("invocation state");
        match &mut *state {
            State::Preflight(candidate) => { *candidate = Some(parent); Ok(()) }
            _ => Err("Monitor attachment requires active preflight".into()),
        }
    }

    pub(super) fn admit(&self, name: &str, input: &Value) -> Result<(), String> {
        let mut state = self.state.lock().expect("invocation state");
        if matches!(&*state, State::Preflight(None)) && self.name != "monitor" {
            *state = State::Retired;
            return Ok(());
        }
        if self.name != name || self.input != *input {
            *state = State::Retired;
            return Err("Monitor invocation identity changed".into());
        }
        match std::mem::replace(&mut *state, State::Retired) {
            State::Preflight(parent) => { *state = State::Executable(parent); Ok(()) }
            _ => Err("Monitor invocation cannot be admitted twice".into()),
        }
    }

    pub(super) fn take(&self, id: &str, input: &Value, cwd: &std::path::Path) -> Result<Option<PathBuf>, String> {
        self.matches(id, input, cwd)?;
        let mut state = self.state.lock().expect("invocation state");
        match &mut *state {
            State::Executable(parent) => Ok(parent.take()),
            _ => Err("Monitor parent is unavailable before admission or after retirement".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn nested_same_id_invocations_are_isolated_and_take_requires_admission() {
        let input = serde_json::json!({"path":"fixture"});
        let outer = Invocation::new(1, 0, "same".into(), "monitor".into(), input.clone(), "/cwd".into());
        let inner = Invocation::new(1, 0, "same".into(), "monitor".into(), input.clone(), "/cwd".into());
        let retirement = Retirement(outer.clone());
        CURRENT.scope(outer.clone(), async {
            outer.attach("same", &input, std::path::Path::new("/cwd"), "/outer".into()).expect("attach");
            assert!(outer.take("same", &input, std::path::Path::new("/cwd")).is_err());
            CURRENT.scope(inner.clone(), async {
                assert!(CURRENT.with(|current| Arc::ptr_eq(current, &inner)));
                inner.admit("monitor", &input).expect("inner admission");
                assert_eq!(inner.take("same", &input, std::path::Path::new("/cwd")).expect("inner take"), None);
            }).await;
            assert!(CURRENT.with(|current| Arc::ptr_eq(current, &outer)));
            outer.admit("monitor", &input).expect("outer admission");
            assert_eq!(outer.take("same", &input, std::path::Path::new("/cwd")).expect("outer take"), Some("/outer".into()));
            assert_eq!(outer.take("same", &input, std::path::Path::new("/cwd")).expect("consumed"), None);
        }).await;
        drop(retirement);
        assert!(outer.take("same", &input, std::path::Path::new("/cwd")).is_err());
    }

    #[test]
    fn changed_input_retires_candidate_and_json_cannot_authorize() {
        let input = serde_json::json!({"path":"fixture","approvedParent":"/forged"});
        let scope = Invocation::new(1, 0, "id".into(), "monitor".into(), input.clone(), "/cwd".into());
        scope.admit("monitor", &input).expect("admission");
        assert_eq!(scope.take("id", &input, std::path::Path::new("/cwd")).expect("no attachment"), None);
        assert!(scope.take("id", &serde_json::json!({"path":"changed"}), std::path::Path::new("/cwd")).is_err());
        assert!(scope.take("id", &input, std::path::Path::new("/cwd")).is_err());
    }

    #[test]
    fn attached_approval_preserves_identity_fence_for_other_tool_names() {
        let input = serde_json::json!({"path":"fixture"});
        let scope = Invocation::new(1, 0, "id".into(), "monitor_fixture".into(), input.clone(), "/cwd".into());
        scope.attach("id", &input, std::path::Path::new("/cwd"), "/approved".into()).expect("attachment");
        assert!(scope.admit("monitor_fixture", &serde_json::json!({"path":"changed"})).is_err());
        assert!(scope.take("id", &input, std::path::Path::new("/cwd")).is_err());
    }

    #[test]
    fn attached_monitor_changed_input_retires_approval() {
        let input = serde_json::json!({"path":"fixture"});
        let scope = Invocation::new(1, 0, "id".into(), "monitor".into(), input.clone(), "/cwd".into());
        scope.attach("id", &input, std::path::Path::new("/cwd"), "/approved".into()).expect("attachment");
        assert!(scope.admit("monitor", &serde_json::json!({"path":"changed"})).is_err());
        assert!(scope.take("id", &input, std::path::Path::new("/cwd")).is_err());
        assert!(scope.admit("monitor", &input).is_err());
    }

    #[tokio::test]
    async fn dropping_pending_execution_retires_approval() {
        use std::{future::Future, task::Poll};
        let input = serde_json::json!({"path":"fixture"});
        let invocation = Invocation::new(1, 0, "id".into(), "monitor".into(), input.clone(), "/cwd".into());
        invocation.attach("id", &input, std::path::Path::new("/cwd"), "/approved".into()).expect("attachment");
        invocation.admit("monitor", &input).expect("admission");
        let retirement = Retirement(invocation.clone());
        let mut execution = Box::pin(CURRENT.scope(invocation.clone(), async move {
            let _retirement = retirement;
            std::future::pending::<()>().await;
        }));
        std::future::poll_fn(|cx| {
            assert!(execution.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        }).await;
        drop(execution);
        assert!(invocation.take("id", &input, std::path::Path::new("/cwd")).is_err());
        assert!(CURRENT.try_with(|_| ()).is_err());
    }

    #[tokio::test]
    async fn parallel_same_id_invocations_keep_separate_approval() {
        let input = serde_json::json!({"path":"fixture"});
        let first = Invocation::new(1, 0, "id".into(), "monitor".into(), input.clone(), "/cwd".into());
        let second = Invocation::new(1, 0, "id".into(), "monitor".into(), input.clone(), "/cwd".into());
        let rendezvous = Arc::new(tokio::sync::Barrier::new(2));
        let execute = |invocation: Arc<Invocation>, parent: PathBuf| {
            let input = input.clone();
            let rendezvous = rendezvous.clone();
            async move {
                let _retirement = Retirement(invocation.clone());
                CURRENT.scope(invocation.clone(), async {
                    invocation.attach("id", &input, std::path::Path::new("/cwd"), parent.clone()).expect("attachment");
                    rendezvous.wait().await;
                    invocation.admit("monitor", &input).expect("admission");
                    assert!(CURRENT.with(|current| Arc::ptr_eq(current, &invocation)));
                    invocation.take("id", &input, std::path::Path::new("/cwd")).expect("take")
                }).await
            }
        };
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            tokio::join!(execute(first, "/first".into()), execute(second, "/second".into()))
        }).await.expect("bounded concurrent admission");
        assert_eq!(result, (Some("/first".into()), Some("/second".into())));
    }
}
