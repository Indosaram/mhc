//! Port of senpi `packages/agent/src/execution/grant-verify.ts`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

/// The capability a hand requires: a tool call is `execute`.
pub const HAND_CAPABILITY: &str = "execute";

/// The workspace label of a configured root that names none (0009's default workspace).
pub const DEFAULT_WORKSPACE: &str = "omo";

/// One 0009 grant row: `holder` may do `capability` on `target` in `workspace`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrantRow {
    pub holder: String,
    pub target: String,
    pub workspace: String,
    pub capability: String,
}

/// A verified roster's grants as the hand holds them. `expires_at` is the roster's `exp_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GrantSnapshot {
    pub rv: u64,
    pub expires_at: i64,
    pub grants: Vec<GrantRow>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrantRefusal {
    GrantMissing,
    GrantExpired,
    WorkspaceUnknown,
}

impl GrantRefusal {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::GrantMissing => "grant_missing",
            Self::GrantExpired => "grant_expired",
            Self::WorkspaceUnknown => "workspace_unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GrantVerdict {
    Ok { root: String },
    Denied { reason: GrantRefusal },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRoot {
    pub root: String,
    pub label: Option<String>,
}

pub type CanonicalizeFn = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;
pub type ReplaceListener = Arc<dyn Fn() + Send + Sync>;

pub struct GrantVerifierOptions {
    pub self_id: String,
    pub workspaces: Vec<WorkspaceRoot>,
    pub canonicalize: CanonicalizeFn,
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

/// Offline check of the sender's capability grant from a local snapshot of the verified roster.
pub trait GrantVerifier: Send + Sync {
    fn verify(&self, peer: &str, scope_root: &str) -> GrantVerdict;
    fn install(&self, snapshot: GrantSnapshot) -> Result<(), String>;
    fn on_replace(&self, listener: ReplaceListener) -> Box<dyn FnOnce() + Send + Sync>;
}

/// Whose calls an executor runs: its own device's (`SelfHost`) or one authenticated peer's.
#[derive(Clone)]
pub enum HandAuthority {
    SelfHost,
    Peer {
        peer: String,
        verifier: Arc<dyn GrantVerifier>,
    },
}

pub fn authorize(authority: &HandAuthority, scope_root: &str) -> GrantVerdict {
    match authority {
        HandAuthority::SelfHost => GrantVerdict::Ok {
            root: scope_root.to_owned(),
        },
        HandAuthority::Peer { peer, verifier } => verifier.verify(peer, scope_root),
    }
}

/// `child` is `root` or lies under it, compared by whole path components (`/ws` never contains `/ws2`).
fn contains(root: &str, child: &str) -> bool {
    let base = root.trim_end_matches(['/', '\\']);
    if child == base {
        return true;
    }
    if !child.starts_with(base) {
        return false;
    }
    matches!(child[base.len()..].chars().next(), Some('/' | '\\'))
}

struct VerifierState {
    snapshot: Option<GrantSnapshot>,
    next_listener_id: u64,
    listeners: HashMap<u64, ReplaceListener>,
}

pub struct DefaultGrantVerifier {
    self_id: String,
    workspaces: Vec<WorkspaceRoot>,
    canonicalize: CanonicalizeFn,
    now: Arc<dyn Fn() -> i64 + Send + Sync>,
    state: Arc<Mutex<VerifierState>>,
}

impl DefaultGrantVerifier {
    fn canonical(&self, path: &str) -> Option<String> {
        (self.canonicalize)(path).ok()
    }

    fn workspace_of(&self, root: &str) -> Option<String> {
        let mut best: Option<(String, usize)> = None;
        for workspace in &self.workspaces {
            let Some(resolved) = self.canonical(&workspace.root) else {
                continue;
            };
            if !contains(&resolved, root) {
                continue;
            }
            let len = resolved.len();
            if best.as_ref().is_none_or(|(_, best_len)| len > *best_len) {
                let label = workspace
                    .label
                    .clone()
                    .unwrap_or_else(|| DEFAULT_WORKSPACE.to_owned());
                best = Some((label, len));
            }
        }
        best.map(|(label, _)| label)
    }
}

impl GrantVerifier for DefaultGrantVerifier {
    fn verify(&self, peer: &str, scope_root: &str) -> GrantVerdict {
        let Some(root) = self.canonical(scope_root) else {
            return GrantVerdict::Denied {
                reason: GrantRefusal::WorkspaceUnknown,
            };
        };
        let Some(workspace) = self.workspace_of(&root) else {
            return GrantVerdict::Denied {
                reason: GrantRefusal::WorkspaceUnknown,
            };
        };
        if peer == self.self_id {
            return GrantVerdict::Ok { root };
        }
        let guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let Some(current) = &guard.snapshot else {
            return GrantVerdict::Denied {
                reason: GrantRefusal::GrantExpired,
            };
        };
        if (self.now)() >= current.expires_at {
            return GrantVerdict::Denied {
                reason: GrantRefusal::GrantExpired,
            };
        }
        let granted = current.grants.iter().any(|row| {
            row.holder == peer
                && row.target == self.self_id
                && row.workspace == workspace
                && row.capability == HAND_CAPABILITY
        });
        if granted {
            GrantVerdict::Ok { root }
        } else {
            GrantVerdict::Denied {
                reason: GrantRefusal::GrantMissing,
            }
        }
    }

    fn install(&self, next: GrantSnapshot) -> Result<(), String> {
        let listeners: Vec<ReplaceListener> = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(current) = &guard.snapshot
                && next.rv < current.rv
            {
                return Ok(());
            }
            guard.snapshot = Some(next);
            guard.listeners.values().cloned().collect()
        };
        for listener in listeners {
            listener();
        }
        Ok(())
    }

    fn on_replace(&self, listener: ReplaceListener) -> Box<dyn FnOnce() + Send + Sync> {
        let id = {
            let mut guard = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let id = guard.next_listener_id;
            guard.next_listener_id += 1;
            guard.listeners.insert(id, listener);
            id
        };
        let state = Arc::clone(&self.state);
        Box::new(move || {
            state
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .listeners
                .remove(&id);
        })
    }
}

pub fn create_grant_verifier(options: GrantVerifierOptions) -> Arc<DefaultGrantVerifier> {
    let now = options.now.unwrap_or_else(|| {
        Arc::new(|| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_millis() as i64)
        })
    });
    Arc::new(DefaultGrantVerifier {
        self_id: options.self_id,
        workspaces: options.workspaces,
        canonicalize: options.canonicalize,
        now,
        state: Arc::new(Mutex::new(VerifierState {
            snapshot: None,
            next_listener_id: 1,
            listeners: HashMap::new(),
        })),
    })
}
