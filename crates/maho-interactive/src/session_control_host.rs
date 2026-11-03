//! Port of senpi packages/coding-agent/src/modes/interactive/session-control-host.ts (v2026.10.27-2).
//!
//! What the interactive mode installs on its session so an extension can register a control
//! endpoint. It holds at most one endpoint per process.
//!
//! It also turns editor callbacks into the two editor edges. `draft_cleared` must not fire for a
//! submission, and the editor clears itself (`on_change("")`) synchronously BEFORE it calls
//! `on_submit`, so the clear is judged one task turn later, after the submission had its chance to
//! claim it.
//!
//! A delivery never runs ahead of input the user already submitted. Every submission opens a
//! ticket, and admission is held while any ticket is open. A branch whose input still has to reach
//! the runtime claims the ticket (`claim_handoff`) - text buffered for the main loop, a steer into a
//! running turn - and releases it when the runtime took the input. Every other submission has been
//! handed to the runtime (or to a built-in) once the handler's synchronous part returns, so its ticket
//! is released right there. Turn starts release nothing. The last ticket's release is the `submission`
//! edge.
use crate::session_control_endpoint::{ControlEndpointOutcome, register_session_control_endpoint};
use crate::session_control_lifecycle::{ActiveControlEndpoint, TuiControlContext};
use futures::future::BoxFuture;
use maho_core::extensions::session_control_types::{
    RegisterControlEndpointOptions, SessionControlRegistration,
};
use maho_core::session_control_actions::{ControlEndpointHost, ControlEndpointHostProvider};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Idempotent: the runtime took this input (or it will never reach the runtime).
#[derive(Clone)]
pub struct SubmissionTicket {
    released: Arc<AtomicBool>,
    host: Arc<HostState>,
}

impl SubmissionTicket {
    pub fn release(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        self.host.open_tickets.fetch_sub(1, Ordering::SeqCst);
        self.host.wake_if_unheld();
    }
}

/// The context minus `editor_revision`, which the host supplies; `None` surface while the pane is
/// still being built.
pub type ControlContextProvider = Arc<
    dyn Fn() -> (
            Arc<maho_core::agent_session::AgentSession>,
            String,
            Option<Arc<dyn crate::session_control_commands::TuiControlSurface>>,
        ) + Send
        + Sync,
>;

struct HostState {
    active: Mutex<Option<ActiveControlEndpoint>>,
    editor_revision: AtomicU64,
    had_draft: AtomicBool,
    submitting: Arc<AtomicBool>,
    open_tickets: AtomicUsize,
    buffered_elsewhere: AtomicBool,
    handoff: Mutex<Option<SubmissionTicket>>,
    registering: tokio::sync::Mutex<()>,
    context: ControlContextProvider,
    /// Whether the bound session offers this host to a registration (senpi
    /// `session.setControlEndpointHost(host | undefined)`).
    offered: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

impl HostState {
    fn active(&self) -> Option<ActiveControlEndpoint> {
        lock(&self.active).clone()
    }

    fn submission_in_flight(&self) -> bool {
        self.open_tickets.load(Ordering::SeqCst) > 0
            || self.buffered_elsewhere.load(Ordering::SeqCst)
    }

    fn wake_if_unheld(self: &Arc<Self>) {
        // Deferred: a prompt reports its disposition just before the run marks itself active.
        let host = self.clone();
        spawn_deferred(move || {
            if !host.submission_in_flight()
                && let Some(active) = host.active()
            {
                active.wake("submission");
            }
        });
    }
}

/// senpi `queueMicrotask`: runs after the current synchronous work. Outside a runtime (unit tests of
/// the editor edges) it runs inline.
fn spawn_deferred(task: impl FnOnce() + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(async move {
                tokio::task::yield_now().await;
                task();
            });
        }
        Err(_) => task(),
    }
}

#[derive(Clone)]
pub struct TuiSessionControlHost {
    state: Arc<HostState>,
}

impl TuiSessionControlHost {
    pub fn new(context: ControlContextProvider) -> Self {
        Self {
            state: Arc::new(HostState {
                active: Mutex::new(None),
                editor_revision: AtomicU64::new(0),
                had_draft: AtomicBool::new(false),
                submitting: Arc::new(AtomicBool::new(false)),
                open_tickets: AtomicUsize::new(0),
                buffered_elsewhere: AtomicBool::new(false),
                handoff: Mutex::new(None),
                registering: tokio::sync::Mutex::new(()),
                context,
                offered: AtomicBool::new(false),
            }),
        }
    }

    /// One registration at a time: one still opening when another starts (a retiring extension
    /// generation's, still in flight across a reload, and the new generation's) would otherwise finish
    /// last and replace `active`, leaving the other endpoint serving with no host tracking it
    /// (senpi#3246).
    pub async fn register_endpoint(
        &self,
        options: RegisterControlEndpointOptions,
    ) -> SessionControlRegistration {
        let _registering = self.state.registering.lock().await;
        self.register_now(options).await
    }

    async fn register_now(
        &self,
        options: RegisterControlEndpointOptions,
    ) -> SessionControlRegistration {
        // A previous endpoint whose teardown failed is gone from this host either way: the new
        // registration still gets its endpoint. (The Rust teardown reports its own failures as
        // notices, so there is no rejection to turn into "control endpoint teardown failed".)
        self.dispose_active().await;
        let (session, agent_dir, surface) = (self.state.context)();
        let Some(surface) = surface else {
            return SessionControlRegistration::Failed {
                reason: "the terminal is not ready to serve a control endpoint".into(),
            };
        };
        let revision = self.state.clone();
        let context = TuiControlContext {
            session,
            agent_dir,
            surface,
            editor_revision: Arc::new(move || revision.editor_revision.load(Ordering::SeqCst)),
        };
        let (registration, endpoint) =
            match register_session_control_endpoint(context, options).await {
                ControlEndpointOutcome::NotRegistered { registration } => return registration,
                ControlEndpointOutcome::Registered {
                    registration,
                    endpoint,
                } => (registration, endpoint),
            };
        let SessionControlRegistration::Registered { socket, .. } = registration else {
            return registration;
        };
        *lock(&self.state.active) = Some(endpoint.clone());
        let state = self.state.clone();
        SessionControlRegistration::Registered {
            socket,
            // Its owner disposing it (a retired extension generation, senpi#3246) also takes it off
            // this host, but only once its teardown settled: until then the next registration's
            // `dispose_active` awaits it, so the teardown's header-only discard never removes the file
            // the next endpoint serves.
            dispose: Arc::new(move || -> BoxFuture<'static, Result<(), String>> {
                let endpoint = endpoint.clone();
                let state = state.clone();
                Box::pin(async move {
                    endpoint.dispose().await;
                    let mut active = lock(&state.active);
                    if active
                        .as_ref()
                        .is_some_and(|current| current.socket == endpoint.socket)
                    {
                        *active = None;
                    }
                    Ok(())
                })
            }),
        }
    }

    /// senpi `session.setControlEndpointHost(offered ? host : undefined)`.
    pub fn set_offered(&self, offered: bool) {
        self.state.offered.store(offered, Ordering::SeqCst);
    }

    /// The `host` dependency of core's `create_session_control_actions`: this host while the bound
    /// session offers it, `None` otherwise (then a registration answers `unsupported_mode`). The mode
    /// installs what it yields with `AgentSession::set_control_endpoint_host` when it binds extensions.
    pub fn provider(&self) -> ControlEndpointHostProvider {
        let host = self.clone();
        Arc::new(move || {
            host.state
                .offered
                .load(Ordering::SeqCst)
                .then(|| Arc::new(host.clone()) as Arc<dyn ControlEndpointHost>)
        })
    }

    /// The editor's change edge; `has_draft` reads the composer now.
    pub fn editor_changed(&self, has_draft: bool) {
        let state = &self.state;
        state.editor_revision.fetch_add(1, Ordering::SeqCst);
        let cleared = state.had_draft.swap(has_draft, Ordering::SeqCst) && !has_draft;
        if let Some(active) = state.active() {
            active.note_state();
        }
        if !cleared {
            return;
        }
        let host = state.clone();
        spawn_deferred(move || {
            if !host.submitting.load(Ordering::SeqCst)
                && let Some(active) = host.active()
            {
                active.wake("draft_cleared");
            }
        });
    }

    /// The editor's submit edge, run around the submit handler: opens a ticket the handler may claim.
    pub fn editor_submitted(&self, submit: impl FnOnce()) {
        let state = &self.state;
        state.submitting.store(true, Ordering::SeqCst);
        let submitting = state.submitting.clone();
        spawn_deferred(move || submitting.store(false, Ordering::SeqCst));
        let ticket = self.open_ticket();
        *lock(&state.handoff) = Some(ticket.clone());
        submit();
        let claimed = !lock(&state.handoff)
            .take()
            .is_some_and(|open| Arc::ptr_eq(&open.released, &ticket.released));
        if !claimed {
            ticket.release();
        }
    }

    /// Called synchronously by a submit branch whose input still has to reach the runtime.
    pub fn claim_handoff(&self) -> Option<SubmissionTicket> {
        lock(&self.state.handoff).take()
    }

    pub fn submission_in_flight(&self) -> bool {
        self.state.submission_in_flight()
    }

    /// Input the TUI buffers outside the main loop (the compaction queue) holds admission the same way.
    pub fn note_buffered_elsewhere(&self, held: bool) {
        let released = self.state.buffered_elsewhere.swap(held, Ordering::SeqCst) && !held;
        if released {
            self.state.wake_if_unheld();
        }
    }

    pub fn questions_changed(&self) {
        if let Some(active) = self.state.active() {
            active.note_questions();
        }
    }

    pub async fn dispose_active(&self) {
        let active = lock(&self.state.active).take();
        if let Some(active) = active {
            active.dispose().await;
        }
    }

    fn open_ticket(&self) -> SubmissionTicket {
        self.state.open_tickets.fetch_add(1, Ordering::SeqCst);
        SubmissionTicket {
            released: Arc::new(AtomicBool::new(false)),
            host: self.state.clone(),
        }
    }
}

impl ControlEndpointHost for TuiSessionControlHost {
    fn register(
        &self,
        options: RegisterControlEndpointOptions,
    ) -> BoxFuture<'_, SessionControlRegistration> {
        Box::pin(self.register_endpoint(options))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> TuiSessionControlHost {
        // The editor edges never register an endpoint, so the context is never read.
        let context: ControlContextProvider = Arc::new(|| panic!("no registration in this test"));
        TuiSessionControlHost::new(context)
    }

    #[test]
    fn an_unclaimed_submission_releases_its_ticket_when_the_handler_returns() {
        let host = host();
        host.editor_submitted(|| {});
        assert!(!host.submission_in_flight());
    }

    #[test]
    fn a_claimed_ticket_holds_admission_until_the_runtime_takes_the_input() {
        let host = host();
        let mut claimed = None;
        host.editor_submitted(|| claimed = host.claim_handoff());
        assert!(host.submission_in_flight());
        let ticket = claimed.expect("the branch claimed its ticket");
        ticket.release();
        ticket.release();
        assert!(!host.submission_in_flight());
    }

    #[test]
    fn input_buffered_elsewhere_holds_admission_until_released() {
        let host = host();
        host.note_buffered_elsewhere(true);
        assert!(host.submission_in_flight());
        host.note_buffered_elsewhere(false);
        assert!(!host.submission_in_flight());
    }

    #[test]
    fn the_provider_offers_the_host_only_while_the_session_offers_it() {
        let host = host();
        let provider = host.provider();
        assert!(provider().is_none());
        host.set_offered(true);
        assert!(provider().is_some());
        host.set_offered(false);
        assert!(provider().is_none());
    }

    #[test]
    fn every_editor_change_advances_the_revision() {
        let host = host();
        host.editor_changed(true);
        host.editor_changed(false);
        assert_eq!(host.state.editor_revision.load(Ordering::SeqCst), 2);
    }
}
