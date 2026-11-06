//! Port of `permission-system/dispatch.ts`: the host fence for input-identity approvals.
//!
//! A tool that dispatches a prepared operation (an MCP call) resolves live permission after its
//! own readiness, then checks the returned fence immediately before it sends the operation. An
//! "Allow once" given in the `tool_call` preflight is kept on the call's validated input identity
//! (senpi keys a `WeakMap` on the input object; the agent loop scopes an
//! [`InvocationInput`] over the call instead), so it is honored once for that call only, and only
//! while the authorizer, the operation and the Ask policy that were presented are unchanged.
use crate::types::{Action, PermissionError};
use maho_ext_api::{
    AbortSignal, ExtensionContext, SessionManager, ToolDispatchIdentity,
    invocation_input::{InvocationInput, WeakInvocationInput},
};
use serde_json::Value;
use std::{
    collections::HashMap,
    fmt,
    future::Future,
    pin::Pin,
    sync::{Arc, LazyLock, Mutex},
};

pub type DispatchFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// senpi `DispatchRequest`.
#[derive(Clone, Debug)]
pub struct DispatchRequest {
    pub tool_call_id: String,
    pub parent_tool_call_id: Option<String>,
    pub tool_name: String,
    pub input: Value,
    /// The validated input's identity: the object senpi's `WeakMap` keys approvals on.
    pub input_identity: InvocationInput,
    pub identity: ToolDispatchIdentity,
}

/// senpi `DispatchPolicy`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DispatchPolicy {
    pub action: Action,
    pub fingerprint: String,
}

/// senpi `DispatchAuthorizer`: the permission extension's live evaluator and approval prompt.
pub trait DispatchAuthorizer: Send + Sync {
    /// The live decision for `request`; an error refuses the dispatch (setup failure).
    fn policy(
        &self,
        request: &DispatchRequest,
        ctx: &ExtensionContext,
    ) -> Result<DispatchPolicy, String>;
    /// Ask the user; an error is the refusal reason.
    fn ask<'a>(
        &'a self,
        request: &'a DispatchRequest,
        ctx: &'a ExtensionContext,
        signal: AbortSignal,
    ) -> DispatchFuture<'a>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchError {
    /// No authorizer was ever registered for the session.
    Unavailable,
    /// The registration was retired (extension reloaded or shut down).
    Retired,
    /// The live policy denies the operation (senpi `DeniedError(["*"])`).
    Denied,
    /// The caller's own signal aborted.
    Aborted,
    /// The policy failed or the user refused; the message is the reason.
    Failed(String),
}
impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unavailable => f.write_str("Permission authorizer is unavailable"),
            Self::Retired => f.write_str("Permission authorizer was retired"),
            Self::Denied => write!(f, "{}", PermissionError::Denied(vec!["*".into()])),
            Self::Aborted => f.write_str("Permission request was aborted"),
            Self::Failed(reason) => f.write_str(reason),
        }
    }
}
impl std::error::Error for DispatchError {}

struct Registration {
    authorizer: Arc<dyn DispatchAuthorizer>,
    retired: AbortSignal,
}
struct Approval {
    registration: Arc<Registration>,
    tool_call_id: String,
    tool_name: String,
    input: String,
    identity: ToolDispatchIdentity,
    policy: DispatchPolicy,
}

// senpi keys authorizers on the runner's stable `sessionManager`. DIVERGENCE: the host hands each
// context a session-manager adapter it rebuilds on every UI/core rebind (RPC reattach) without a
// new `session_start`, so the adapter pointer is not stable; the session id is. Retired
// registrations are retained like senpi's, so required authority stays distinguishable from a
// session where enforcement was never loaded. Approvals stay keyed on the live input identity; a
// dead input drops its entry on the next access.
static AUTHORIZERS: LazyLock<Mutex<HashMap<String, Arc<Registration>>>> =
    LazyLock::new(Default::default);
type ApprovalMap = Mutex<HashMap<usize, (WeakInvocationInput, Arc<Approval>)>>;
static APPROVALS: LazyLock<ApprovalMap> = LazyLock::new(Default::default);

fn registration_for(session: &Arc<dyn SessionManager>) -> Option<Arc<Registration>> {
    AUTHORIZERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(session.session_id())
        .map(Arc::clone)
}
fn approval_for(input: &InvocationInput) -> Option<Arc<Approval>> {
    let approvals = APPROVALS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    approvals
        .get(&input.key())
        .filter(|(weak, _)| weak.is_alive())
        .map(|(_, approval)| Arc::clone(approval))
}
fn input_text(input: &Value) -> String {
    input.to_string()
}

/// Retires one registration; it cannot remove the authorizer a newer extension load installed.
pub struct DispatchRetirement(Arc<Registration>);
impl DispatchRetirement {
    pub fn retire(&self) {
        // The retired registration is retained, so required authority cannot become
        // indistinguishable from a session where enforcement was never loaded.
        self.0.retired.abort();
    }
}

/// senpi `registerDispatchAuthorizer`.
pub fn register_dispatch_authorizer(
    session: &Arc<dyn SessionManager>,
    authorizer: Arc<dyn DispatchAuthorizer>,
) -> DispatchRetirement {
    let registration = Arc::new(Registration {
        authorizer,
        retired: AbortSignal::default(),
    });
    let previous = AUTHORIZERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(session.session_id().to_owned(), Arc::clone(&registration));
    if let Some(previous) = previous {
        previous.retired.abort();
    }
    DispatchRetirement(registration)
}

/// senpi `hasDispatchAuthorizer`.
pub fn has_dispatch_authorizer(session: &Arc<dyn SessionManager>) -> bool {
    registration_for(session).is_some()
}

/// A presented approval, recorded only once the user approved it.
pub struct PreparedApproval {
    session: Arc<dyn SessionManager>,
    input: InvocationInput,
    approval: Arc<Approval>,
}
impl PreparedApproval {
    /// Record the approval for this call's input, unless its authorizer was retired meanwhile.
    pub fn approve(self) -> Result<(), DispatchError> {
        let registration = &self.approval.registration;
        if registration.retired.is_aborted() {
            return Err(DispatchError::Retired);
        }
        if !registration_for(&self.session)
            .is_some_and(|current| Arc::ptr_eq(&current, registration))
        {
            return Err(DispatchError::Retired);
        }
        let mut approvals = APPROVALS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        approvals.retain(|_, (weak, _)| weak.is_alive());
        approvals.insert(self.input.key(), (self.input.downgrade(), self.approval));
        Ok(())
    }
}

/// senpi `prepareDispatchApproval`: keep the policy and operation that were actually presented, not
/// their post-reply replacements.
pub fn prepare_dispatch_approval(
    session: &Arc<dyn SessionManager>,
    request: &DispatchRequest,
    policy: DispatchPolicy,
) -> Result<PreparedApproval, DispatchError> {
    let registration = registration_for(session).ok_or(DispatchError::Unavailable)?;
    if registration.retired.is_aborted() {
        return Err(DispatchError::Retired);
    }
    Ok(PreparedApproval {
        session: Arc::clone(session),
        input: request.input_identity.clone(),
        approval: Arc::new(Approval {
            registration,
            tool_call_id: request.tool_call_id.clone(),
            tool_name: request.tool_name.clone(),
            input: input_text(&request.input),
            identity: request.identity.clone(),
            policy,
        }),
    })
}

/// senpi `forgetDispatchApproval`: the call finished, its approval must not outlive it.
pub fn forget_dispatch_approval(input: &InvocationInput) {
    APPROVALS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&input.key());
}

fn matches(
    approval: Option<&Approval>,
    registration: &Arc<Registration>,
    request: &DispatchRequest,
    policy: &DispatchPolicy,
) -> bool {
    approval.is_some_and(|approval| {
        Arc::ptr_eq(&approval.registration, registration)
            && approval.tool_call_id == request.tool_call_id
            && approval.tool_name == request.tool_name
            && approval.input == input_text(&request.input)
            && approval.identity.owner == request.identity.owner
            && approval.identity.metadata == request.identity.metadata
            && approval.policy == *policy
    })
}

async fn aborted(signal: Option<&AbortSignal>) {
    match signal {
        Some(signal) => signal.cancelled().await,
        None => std::future::pending().await,
    }
}

/// The synchronous check that must pass immediately before dispatch; `false` requires preparing
/// again through [`authorize_tool_dispatch`].
pub struct DispatchFence {
    session: Arc<dyn SessionManager>,
    registration: Arc<Registration>,
    signal: Option<AbortSignal>,
    approved_input: String,
    ctx: ExtensionContext,
}
impl fmt::Debug for DispatchFence {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DispatchFence")
            .field("approved_input", &self.approved_input)
            .finish_non_exhaustive()
    }
}
impl DispatchFence {
    /// DIVERGENCE: senpi's fence closes over the mutable request object; Rust requests are owned
    /// values, so the caller passes the request it is about to dispatch.
    pub fn is_current(&self, request: &DispatchRequest) -> bool {
        if self.signal.as_ref().is_some_and(AbortSignal::is_aborted)
            || self.registration.retired.is_aborted()
            || !registration_for(&self.session)
                .is_some_and(|current| Arc::ptr_eq(&current, &self.registration))
        {
            return false;
        }
        if input_text(&request.input) != self.approved_input {
            return false;
        }
        let Ok(current) = self.registration.authorizer.policy(request, &self.ctx) else {
            return false;
        };
        match current.action {
            Action::Deny => false,
            Action::Allow => true,
            Action::Ask => matches(
                approval_for(&request.input_identity).as_deref(),
                &self.registration,
                request,
                &current,
            ),
        }
    }
}

/// senpi `authorizeToolDispatch`: resolve live permission after the tool's readiness. Aborting the
/// caller's `signal` ends only this caller's wait; it does not retire another caller's authority.
pub async fn authorize_tool_dispatch(
    session: &Arc<dyn SessionManager>,
    request: &DispatchRequest,
    ctx: &ExtensionContext,
    signal: Option<AbortSignal>,
) -> Result<DispatchFence, DispatchError> {
    if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
        return Err(DispatchError::Aborted);
    }
    let registration = registration_for(session).ok_or(DispatchError::Unavailable)?;
    if registration.retired.is_aborted() {
        return Err(DispatchError::Retired);
    }
    let policy = registration
        .authorizer
        .policy(request, ctx)
        .map_err(DispatchError::Failed)?;
    if policy.action == Action::Deny {
        return Err(DispatchError::Denied);
    }
    if policy.action == Action::Ask
        && !matches(
            approval_for(&request.input_identity).as_deref(),
            &registration,
            request,
            &policy,
        )
    {
        let approve = prepare_dispatch_approval(session, request, policy)?;
        let wait = AbortSignal::default();
        let answer = tokio::select! {
            biased;
            () = aborted(signal.as_ref()) => Err(DispatchError::Aborted),
            () = registration.retired.cancelled() => Err(DispatchError::Retired),
            answer = registration.authorizer.ask(request, ctx, wait.clone()) => {
                answer.map_err(DispatchError::Failed)
            }
        };
        if answer.is_err() {
            wait.abort();
        }
        answer?;
        if signal.as_ref().is_some_and(AbortSignal::is_aborted) {
            return Err(DispatchError::Aborted);
        }
        approve.approve()?;
    }
    Ok(DispatchFence {
        session: Arc::clone(session),
        registration,
        signal,
        approved_input: input_text(&request.input),
        ctx: ctx.clone(),
    })
}
