//! Port of senpi test/permission/dispatch.test.ts against dispatch.rs. The pending approvals are
//! driven by channels the test owns, so every interleaving is explicit (no sleeps).
use crate::context_support::{TestSession, context};
use maho_ext_api::{
    AbortSignal, ExtensionContext, SessionManager, ToolDispatchIdentity, ToolDispatchOwner,
    invocation_input::InvocationInput,
};
use maho_ext_permission_system::{
    dispatch::{
        DispatchAuthorizer, DispatchError, DispatchFuture, DispatchPolicy, DispatchRequest,
        authorize_tool_dispatch, prepare_dispatch_approval, register_dispatch_authorizer,
    },
    types::Action,
};
use serde_json::json;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use tokio::sync::oneshot;

/// One scripted reply to an approval prompt: the test learns the prompt entered, then answers.
struct Gate {
    entered: oneshot::Sender<()>,
    reply: oneshot::Receiver<Result<(), String>>,
}
struct Authorizer {
    policy: Mutex<DispatchPolicy>,
    asks: AtomicUsize,
    gates: Mutex<Vec<Gate>>,
}
impl Authorizer {
    fn new(action: Action, fingerprint: &str) -> Arc<Self> {
        Arc::new(Self {
            policy: Mutex::new(policy(action, fingerprint)),
            asks: AtomicUsize::new(0),
            gates: Mutex::new(Vec::new()),
        })
    }
    fn set_policy(&self, next: DispatchPolicy) {
        *self.policy.lock().expect("policy") = next;
    }
    fn asks(&self) -> usize {
        self.asks.load(Ordering::SeqCst)
    }
    /// The next prompt waits for the returned reply; `entered` resolves once it is waiting.
    fn hold_next(&self) -> (oneshot::Receiver<()>, oneshot::Sender<Result<(), String>>) {
        let (entered, entered_rx) = oneshot::channel();
        let (reply_tx, reply) = oneshot::channel();
        self.gates
            .lock()
            .expect("gates")
            .push(Gate { entered, reply });
        (entered_rx, reply_tx)
    }
}
impl DispatchAuthorizer for Authorizer {
    fn policy(&self, _: &DispatchRequest, _: &ExtensionContext) -> Result<DispatchPolicy, String> {
        Ok(self.policy.lock().expect("policy").clone())
    }
    fn ask<'a>(
        &'a self,
        _: &'a DispatchRequest,
        _: &'a ExtensionContext,
        _: AbortSignal,
    ) -> DispatchFuture<'a> {
        self.asks.fetch_add(1, Ordering::SeqCst);
        let gate = self.gates.lock().expect("gates").pop();
        Box::pin(async move {
            let Some(gate) = gate else { return Ok(()) };
            let _ = gate.entered.send(());
            gate.reply
                .await
                .unwrap_or_else(|_| Err("reply dropped".into()))
        })
    }
}
fn policy(action: Action, fingerprint: &str) -> DispatchPolicy {
    DispatchPolicy {
        action,
        fingerprint: fingerprint.into(),
    }
}

struct Fixture {
    session: Arc<dyn SessionManager>,
    ctx: ExtensionContext,
    authorizer: Arc<Authorizer>,
    retire: maho_ext_permission_system::dispatch::DispatchRetirement,
    owner: ToolDispatchOwner,
    _cwd: tempfile::TempDir,
}
fn fixture() -> Fixture {
    let cwd = tempfile::tempdir().expect("cwd");
    // Authorizers are keyed on the session id, so every fixture owns its own session.
    static NEXT_SESSION: AtomicUsize = AtomicUsize::new(0);
    let id = NEXT_SESSION.fetch_add(1, Ordering::SeqCst);
    let session: Arc<dyn SessionManager> = Arc::new(TestSession(format!("dispatch-{id}")));
    let ctx = context(Arc::clone(&session), cwd.path());
    let authorizer = Authorizer::new(Action::Ask, "explicit-ask");
    let retire = register_dispatch_authorizer(&session, authorizer.clone());
    Fixture {
        session,
        ctx,
        authorizer,
        retire,
        owner: ToolDispatchOwner::new(),
        _cwd: cwd,
    }
}
impl Fixture {
    /// A fresh validated input (its own identity) for the same provider ID and operation.
    fn request(&self) -> DispatchRequest {
        DispatchRequest {
            tool_call_id: "provider-id".into(),
            parent_tool_call_id: None,
            tool_name: "mcp_fixture_write".into(),
            input: json!({"value":"same"}),
            input_identity: InvocationInput::new(),
            identity: ToolDispatchIdentity {
                owner: self.owner.clone(),
                metadata: "original-operation".into(),
                operation: None,
            },
        }
    }
    async fn authorize(
        &self,
        request: &DispatchRequest,
        signal: Option<AbortSignal>,
    ) -> Result<maho_ext_permission_system::dispatch::DispatchFence, DispatchError> {
        authorize_tool_dispatch(&self.session, request, &self.ctx, signal).await
    }
}

#[tokio::test]
async fn keeps_one_unchanged_once_approval_while_the_same_invocation_prepares_again() {
    let fx = fixture();
    let call = fx.request();
    let first = fx.authorize(&call, None).await.expect("first");
    let second = fx.authorize(&call, None).await.expect("second");
    let mut writes = 0;
    if first.is_current(&call) && second.is_current(&call) {
        writes += 1;
    }
    assert_eq!((writes, fx.authorizer.asks()), (1, 1));
}

#[tokio::test]
async fn does_not_share_once_approval_between_identical_calls_with_the_same_provider_id() {
    let fx = fixture();
    let (first, second) = (fx.request(), fx.request());
    let fences = [
        (fx.authorize(&first, None).await.expect("first"), &first),
        (fx.authorize(&second, None).await.expect("second"), &second),
    ];
    let writes = fences
        .iter()
        .filter(|(fence, call)| fence.is_current(call))
        .count();
    assert_eq!((writes, fx.authorizer.asks()), (2, 2));
}

#[tokio::test]
async fn refuses_a_newly_denied_operation_after_readiness_without_sending_it() {
    let fx = fixture();
    let call = fx.request();
    let fence = fx.authorize(&call, None).await.expect("approved");
    fx.authorizer.set_policy(policy(Action::Deny, "new-deny"));
    assert!(!fence.is_current(&call));
    let refused = fx.authorize(&call, None).await.expect_err("denied");
    assert_eq!(refused, DispatchError::Denied);
    assert!(refused.to_string().starts_with(
        "The user has specified a rule which prevents you from using this specific tool call."
    ));
}

#[tokio::test]
async fn asks_again_when_the_operation_or_relevant_ask_policy_changes() {
    let fx = fixture();
    let call = fx.request();
    let fence = fx.authorize(&call, None).await.expect("approved");
    fx.authorizer.set_policy(policy(Action::Ask, "new-ask"));
    assert!(!fence.is_current(&call));
    fx.authorize(&call, None).await.expect("asked again");
    let mut replaced = call.clone();
    replaced.identity.owner = ToolDispatchOwner::new();
    let current = fx.authorize(&replaced, None).await.expect("new owner");
    let writes = usize::from(current.is_current(&replaced));
    assert_eq!((fx.authorizer.asks(), writes), (3, 1));
}

#[tokio::test]
async fn does_not_approve_metadata_that_changed_while_the_approval_was_pending() {
    let fx = fixture();
    let (entered, reply) = fx.authorizer.hold_next();
    let mut call = fx.request();
    let pending = fx.authorize(&call, None);
    let (fence, ()) = tokio::join!(pending, async {
        entered.await.expect("prompt entered");
        reply.send(Ok(())).expect("reply");
    });
    let fence = fence.expect("approved original");
    call.input = json!({"value":"different-operation"});
    assert!(!fence.is_current(&call));
    fx.authorize(&call, None)
        .await
        .expect("asked for the change");
    assert_eq!(fx.authorizer.asks(), 2);
}

#[tokio::test]
async fn rejects_pending_approval_when_its_authorizer_is_retired() {
    let fx = fixture();
    let (entered, _reply) = fx.authorizer.hold_next();
    let call = fx.request();
    let (result, ()) = tokio::join!(fx.authorize(&call, None), async {
        entered.await.expect("prompt entered");
        fx.retire.retire();
    });
    let error = result.expect_err("retired");
    assert_eq!(error.to_string(), "Permission authorizer was retired");
}

#[tokio::test]
async fn does_not_revive_old_approval_when_the_same_callback_object_is_registered_again() {
    let fx = fixture();
    let call = fx.request();
    let old = fx.authorize(&call, None).await.expect("approved");
    let _replacement = register_dispatch_authorizer(&fx.session, fx.authorizer.clone());
    fx.retire.retire();
    assert!(!old.is_current(&call));
    let current = fx.authorize(&call, None).await.expect("re-approved");
    let writes = usize::from(current.is_current(&call));
    assert_eq!((writes, fx.authorizer.asks()), (1, 2));
}

#[tokio::test]
async fn does_not_let_cleanup_of_the_old_registration_retire_its_replacement() {
    let fx = fixture();
    let call = fx.request();
    let old = fx.authorize(&call, None).await.expect("approved");
    let allow = Authorizer::new(Action::Allow, "allow");
    let _replacement = register_dispatch_authorizer(&fx.session, allow.clone());
    fx.retire.retire();
    assert!(!old.is_current(&call));
    let next = fx.request();
    let current = fx.authorize(&next, None).await.expect("allowed");
    let writes = usize::from(current.is_current(&next));
    assert_eq!((writes, allow.asks()), (1, 0));
}

#[tokio::test]
async fn aborts_only_the_selected_approval_waiter() {
    let fx = fixture();
    let (entered, _reply) = fx.authorizer.hold_next();
    let abort = AbortSignal::default();
    let selected = fx.request();
    let (result, ()) = tokio::join!(fx.authorize(&selected, Some(abort.clone())), async {
        entered.await.expect("prompt entered");
        abort.abort();
    });
    assert_eq!(result.expect_err("aborted"), DispatchError::Aborted);
    let other = fx.request();
    let current = fx.authorize(&other, None).await.expect("unaffected");
    assert!(current.is_current(&other));
}

#[tokio::test]
async fn keeps_each_preflight_approval_on_its_own_validated_input() {
    // The `tool_call` preflight approves each call on its own input; execution then dispatches
    // without asking again, once per call even for identical provider IDs and arguments.
    let fx = fixture();
    let calls = [fx.request(), fx.request()];
    let mut prompts = 0;
    for call in &calls {
        let presented = policy(Action::Ask, "explicit-ask");
        prepare_dispatch_approval(&fx.session, call, presented)
            .expect("prepared")
            .approve()
            .expect("approved");
        prompts += 1;
    }
    let mut writes = 0;
    for call in &calls {
        let fence = fx.authorize(call, None).await.expect("preflight approved");
        writes += usize::from(fence.is_current(call));
    }
    assert_eq!((writes, prompts, fx.authorizer.asks()), (2, 2, 0));
}

#[tokio::test]
async fn authorizes_through_a_rebound_adapter_for_the_same_session_id() {
    // The host rebuilds the context's session-manager adapter on a UI/core rebind (RPC reattach)
    // without a new session_start; the registration must follow the session, not the adapter.
    let fx = fixture();
    let rebound: Arc<dyn SessionManager> =
        Arc::new(TestSession(fx.session.session_id().to_owned()));
    assert!(!Arc::ptr_eq(&rebound, &fx.session));
    let rebound_ctx = context(Arc::clone(&rebound), fx._cwd.path());
    assert!(maho_ext_permission_system::dispatch::has_dispatch_authorizer(&rebound));
    let call = fx.request();
    let fence = authorize_tool_dispatch(&rebound, &call, &rebound_ctx, None)
        .await
        .expect("registered authorizer found through the rebound adapter");
    assert!(fence.is_current(&call));
    assert_eq!(fx.authorizer.asks(), 1);
    // Retiring the registration made through the first adapter retires it for the rebound one.
    fx.retire.retire();
    assert!(!fence.is_current(&call));
    assert_eq!(
        authorize_tool_dispatch(&rebound, &fx.request(), &rebound_ctx, None)
            .await
            .expect_err("retired"),
        DispatchError::Retired
    );
}
