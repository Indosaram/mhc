use super::interrupt_bounds::WorkerRetirement;
use super::permission_sessions::{
    PermissionSession, register_permission_kernel, unregister_permission_kernel,
};
use crate::kernels::shared::cell_authority_registry::{
    CellRule, InvocationRegistration, InvocationSessionSnapshot, RevokeFuture, SessionKernelControl,
};
use crate::kernels::shared::invocation_authority::{
    CrossSessionAuthorityError, invocation_session, register_invocation_session,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Arc, Mutex};

pub use super::permission_sessions::revoke_session_kernels;

/// Host-side authority token minted per cell run or cross-session kernel tool invocation
/// (`CellAuthorityToken` in `kernel-contract.ts` / `permission-host.ts`).
#[derive(Clone, Debug)]
pub struct CellAuthorityToken {
    pub id: String,
    pub session_id: String,
    pub cwd: String,
    pub authority: Arc<AtomicI32>,
    pub rules: Vec<CellRule>,
}

impl CellAuthorityToken {
    pub fn word(&self) -> i32 {
        self.authority.load(Ordering::SeqCst)
    }

    pub fn is_live(&self) -> bool {
        (self.word() & 2) != 0
    }

    pub fn is_enforced(&self) -> bool {
        (self.word() & 1) != 0
    }

    pub fn kill(&self) {
        self.authority.fetch_and(!2, Ordering::SeqCst);
    }

    pub fn wire_json(&self) -> Value {
        json!({
            "id": self.id,
            "sessionId": self.session_id,
            "cwd": self.cwd,
            "word": self.word(),
            "rules": self.rules,
        })
    }
}

pub type PermissionStateReader = Arc<dyn Fn() -> (u32, bool, Vec<CellRule>) + Send + Sync>;
pub type ResolveSessionReader =
    Arc<dyn Fn(&str) -> Option<InvocationSessionSnapshot> + Send + Sync>;
pub type RetireWorkerFn = Arc<dyn Fn() -> RevokeWorkerFuture + Send + Sync>;
pub type RevokeWorkerFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = WorkerRetirement> + Send>>;

#[derive(Clone)]
pub struct KernelPermissionHostOptions {
    pub session_id: String,
    pub cwd: String,
    pub read_state: Option<PermissionStateReader>,
    pub resolve_session: Option<ResolveSessionReader>,
}

#[derive(Default)]
struct HostState {
    tokens: HashMap<String, CellAuthorityToken>,
    runs: HashMap<String, CellAuthorityToken>,
    invocations: HashMap<String, CellAuthorityToken>,
    invoker_bindings: HashMap<String, usize>,
    retained: HashMap<String, HashMap<String, CellAuthorityToken>>,
    revoked: bool,
    retiring: bool,
}

/// Host authority state machine for JS cell runs & cross-session kernel tool invocations
/// (`permission-host.ts`).
pub struct KernelPermissionHost {
    kernel_id: String,
    options: KernelPermissionHostOptions,
    retire: RetireWorkerFn,
    session: Mutex<Option<PermissionSession>>,
    _release_invocation: Mutex<Option<InvocationRegistration>>,
    state: Mutex<HostState>,
}

impl KernelPermissionHost {
    pub fn new(options: KernelPermissionHostOptions, retire: RetireWorkerFn) -> Arc<Self> {
        let kernel_id = uuid::Uuid::new_v4().to_string();
        let host = Arc::new(Self {
            kernel_id,
            options: options.clone(),
            retire,
            session: Mutex::new(None),
            _release_invocation: Mutex::new(None),
            state: Mutex::new(HostState::default()),
        });
        let control: Arc<dyn SessionKernelControl> =
            Arc::clone(&host) as Arc<dyn SessionKernelControl>;
        let session = register_permission_kernel(&options.session_id, control);
        if let Ok(mut guard) = host.session.lock() {
            *guard = Some(session);
        }
        let weak = Arc::downgrade(&host);
        let registration = register_invocation_session(
            &options.session_id,
            Arc::new(move || match weak.upgrade() {
                Some(host) => host.snapshot(),
                None => InvocationSessionSnapshot {
                    cwd: String::new(),
                    epoch: 0,
                    active: false,
                    rules: Vec::new(),
                },
            }),
        );
        if let Ok(mut guard) = host._release_invocation.lock() {
            *guard = Some(registration);
        }
        host
    }

    pub fn kernel_id(&self) -> &str {
        &self.kernel_id
    }

    pub fn blocked(&self) -> bool {
        let state_blocked = self
            .state
            .lock()
            .map(|guard| guard.revoked || guard.retiring)
            .unwrap_or(true);
        let session_blocked = self
            .session
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(|s| s.dead() || s.pending()))
            .unwrap_or(true);
        state_blocked || session_blocked
    }

    pub fn assert_session_live(&self) -> Result<(), String> {
        let dead = self
            .session
            .lock()
            .ok()
            .and_then(|guard| guard.as_ref().map(PermissionSession::dead))
            .unwrap_or(true);
        if dead {
            Err("Permission authority unavailable; restart the session".into())
        } else {
            Ok(())
        }
    }

    pub fn snapshot(&self) -> InvocationSessionSnapshot {
        let (epoch, active, rules) = match &self.options.read_state {
            Some(read) => read(),
            None => (0, false, Vec::new()),
        };
        InvocationSessionSnapshot {
            cwd: self.options.cwd.clone(),
            epoch,
            active,
            rules,
        }
    }

    fn mint(
        &self,
        session_id: &str,
        cwd: &str,
        epoch: u32,
        active: bool,
        rules: Vec<CellRule>,
    ) -> Result<CellAuthorityToken, String> {
        if epoch >= (1 << 29) {
            return Err("Invalid permission epoch".into());
        }
        let word = ((epoch as i32) << 2) | 2 | i32::from(active);
        Ok(CellAuthorityToken {
            id: uuid::Uuid::new_v4().to_string(),
            session_id: session_id.to_owned(),
            cwd: cwd.to_owned(),
            authority: Arc::new(AtomicI32::new(word)),
            rules,
        })
    }

    /// Resolves host-only caller identity before admitting any invocation frame to the worker.
    pub fn resolve_invocation_token(
        self: &Arc<Self>,
        invoker_session_id: Option<&str>,
    ) -> Result<CellAuthorityToken, CrossSessionAuthorityError> {
        let owner = self.snapshot();
        let resolved = invocation_session(
            &self.options.session_id,
            &owner,
            invoker_session_id,
            self.options.resolve_session.as_ref().map(|resolver| {
                resolver.as_ref() as &dyn Fn(&str) -> Option<InvocationSessionSnapshot>
            }),
        )?;
        let token = self
            .mint(
                &resolved.session_id,
                &resolved.cwd,
                resolved.epoch,
                resolved.active,
                resolved.rules,
            )
            .map_err(|message| CrossSessionAuthorityError {
                code: "cell_permission_denied",
                enforcing_session_id: resolved.session_id.clone(),
                rule: None,
                message,
            })?;
        let mut guard = self.state.lock().map_err(|_| CrossSessionAuthorityError {
            code: "cell_permission_denied",
            enforcing_session_id: resolved.session_id.clone(),
            rule: None,
            message: "Permission authority lock poisoned".into(),
        })?;
        if resolved.session_id != self.options.session_id {
            let control: Arc<dyn SessionKernelControl> =
                Arc::clone(self) as Arc<dyn SessionKernelControl>;
            let binding = register_permission_kernel(&resolved.session_id, control);
            if binding.dead() || binding.pending() {
                if !guard.invoker_bindings.contains_key(&resolved.session_id) {
                    unregister_permission_kernel(&resolved.session_id, &self.kernel_id);
                }
                return Err(CrossSessionAuthorityError {
                    code: "cell_permission_denied",
                    enforcing_session_id: resolved.session_id,
                    rule: None,
                    message: "Permission authority unavailable".into(),
                });
            }
            let entry = guard
                .invoker_bindings
                .entry(resolved.session_id.clone())
                .or_insert(0);
            *entry += 1;
        }
        guard.tokens.insert(token.id.clone(), token.clone());
        Ok(token)
    }

    pub fn begin_run(&self, cell_id: &str) -> Result<CellAuthorityToken, String> {
        let owner = self.snapshot();
        let token = self.mint(
            &self.options.session_id,
            &owner.cwd,
            owner.epoch,
            owner.active,
            owner.rules,
        )?;
        let mut guard = self
            .state
            .lock()
            .map_err(|_| "Permission host state poisoned".to_string())?;
        guard.tokens.insert(token.id.clone(), token.clone());
        guard.runs.insert(cell_id.to_owned(), token.clone());
        Ok(token)
    }

    pub fn settle_run(&self, cell_id: &str) {
        let Ok(mut guard) = self.state.lock() else {
            return;
        };
        if let Some(token) = guard.runs.remove(cell_id) {
            release_token(&mut guard, &token, &self.kernel_id);
        }
    }

    pub fn interrupt_all(&self) {
        let Ok(mut guard) = self.state.lock() else {
            return;
        };
        let tokens: Vec<CellAuthorityToken> = guard.tokens.values().cloned().collect();
        for token in tokens {
            release_token(&mut guard, &token, &self.kernel_id);
        }
    }

    pub fn begin_invocation(
        self: &Arc<Self>,
        call_id: &str,
        invoker_session_id: Option<&str>,
    ) -> Result<CellAuthorityToken, CrossSessionAuthorityError> {
        let token = self.resolve_invocation_token(invoker_session_id)?;
        if let Ok(mut guard) = self.state.lock() {
            guard.invocations.insert(call_id.to_owned(), token.clone());
        }
        Ok(token)
    }

    pub fn end_invocation(&self, call_id: &str) {
        let Ok(mut guard) = self.state.lock() else {
            return;
        };
        if let Some(token) = guard.invocations.remove(call_id) {
            release_token(&mut guard, &token, &self.kernel_id);
        }
    }

    pub fn dispose(&self) {
        self.revoke();
        if let Ok(mut reg) = self._release_invocation.lock() {
            *reg = None;
        }
        let invoker_sessions: Vec<String> = if let Ok(mut guard) = self.state.lock() {
            let tokens: Vec<CellAuthorityToken> = guard.tokens.values().cloned().collect();
            for token in tokens {
                token.kill();
            }
            guard.tokens.clear();
            guard.runs.clear();
            guard.invocations.clear();
            for held in guard.retained.values() {
                for token in held.values() {
                    token.kill();
                }
            }
            guard.retained.clear();
            guard.invoker_bindings.drain().map(|(id, _)| id).collect()
        } else {
            Vec::new()
        };
        for session_id in invoker_sessions {
            unregister_permission_kernel(&session_id, &self.kernel_id);
        }
        unregister_permission_kernel(&self.options.session_id, &self.kernel_id);
    }

    pub fn token(&self, id: &str) -> Option<CellAuthorityToken> {
        self.state
            .lock()
            .ok()
            .and_then(|guard| guard.tokens.get(id).cloned())
    }
}

fn release_token(state: &mut HostState, token: &CellAuthorityToken, kernel_id: &str) {
    if state.tokens.remove(&token.id).is_none() {
        return;
    }
    let word = token.word();
    if (word & 2) != 0 && (word & 1) == 0 {
        if state.invoker_bindings.contains_key(&token.session_id) {
            state
                .retained
                .entry(token.session_id.clone())
                .or_default()
                .insert(token.id.clone(), token.clone());
        }
        sweep_retained(state, &token.session_id, kernel_id);
        return;
    }
    token.kill();
    unbind_session(state, &token.session_id, kernel_id);
}

fn sweep_retained(state: &mut HostState, session_id: &str, kernel_id: &str) {
    let dead_ids: Vec<String> = state
        .retained
        .get(session_id)
        .map(|held| {
            held.iter()
                .filter(|(_, token)| !token.is_live())
                .map(|(id, _)| id.clone())
                .collect()
        })
        .unwrap_or_default();
    if dead_ids.is_empty() {
        return;
    }
    let mut empty = false;
    if let Some(held) = state.retained.get_mut(session_id) {
        for id in &dead_ids {
            held.remove(id);
        }
        empty = held.is_empty();
    }
    if empty {
        state.retained.remove(session_id);
    }
    for _ in dead_ids {
        unbind_session(state, session_id, kernel_id);
    }
}

fn unbind_session(state: &mut HostState, session_id: &str, kernel_id: &str) {
    let mut remove = false;
    if let Some(count) = state.invoker_bindings.get_mut(session_id) {
        if *count > 1 {
            *count -= 1;
        } else {
            remove = true;
        }
    }
    if remove {
        state.invoker_bindings.remove(session_id);
        unregister_permission_kernel(session_id, kernel_id);
    }
}

impl SessionKernelControl for KernelPermissionHost {
    fn kernel_id(&self) -> &str {
        &self.kernel_id
    }

    fn revoke(&self) {
        let Ok(mut guard) = self.state.lock() else {
            return;
        };
        guard.revoked = true;
        let seen: HashSet<String> = guard.tokens.keys().cloned().collect();
        for token in guard.tokens.values() {
            token.kill();
        }
        for held in guard.retained.values() {
            for (id, token) in held {
                if !seen.contains(id) {
                    token.kill();
                }
            }
        }
    }

    fn retire(&self) -> RevokeFuture {
        let retire = Arc::clone(&self.retire);
        let dead_on_abandon = self.session.lock().ok().and_then(|guard| guard.clone());
        Box::pin(async move {
            let outcome = retire().await;
            if outcome == WorkerRetirement::Abandoned
                && let Some(session) = dead_on_abandon
            {
                session.mark_dead();
            }
        })
    }
}
