//! SessionHost port and deterministic create id (`thread/control/session-host.ts`).

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::super::contracts::results::ThreadSummary;
use super::super::errors::ThreadToolFailure;
use super::super::tools::internals::{ThreadToolSummary, summary};
use super::super::tools::ports::{BoxFuture, ThreadHostSession};
use super::super::worktree::{ThreadWorktree, WorktreeRequest};

#[derive(Debug, Clone, Default)]
pub struct SessionOpenRequest {
    pub cwd: Option<String>,
    pub session_path: Option<String>,
    pub fork_from: Option<String>,
    pub name: Option<String>,
    pub initial_skills: Option<Vec<String>>,
    pub permission_preset: Option<String>,
    pub provider: Option<String>,
    pub model_id: Option<String>,
    pub thinking_level: Option<String>,
    pub worktree: Option<WorktreeRequest>,
    pub durable_session_id: Option<String>,
}

/// Deterministic UUIDv8-shaped thread id derived from `(caller_id, idempotency_key)` (`#14060`).
pub fn created_thread_id(caller_id: &str, idempotency_key: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"thread_create\0");
    hasher.update(caller_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(idempotency_key.as_bytes());
    let hex = hex::encode(hasher.finalize());
    let nibble = u8::from_str_radix(&hex[16..17], 16).unwrap_or(0);
    let variant = format!("{:x}", (nibble & 0x3) | 0x8);
    format!(
        "{}-{}-8{}-{}{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[13..16],
        variant,
        &hex[17..20],
        &hex[20..32]
    )
}

#[derive(Debug, Clone)]
pub struct HostedThread {
    pub summary: ThreadToolSummary,
    pub worktree: Option<ThreadWorktree>,
    pub warnings: Option<Vec<ThreadToolFailure>>,
}

pub fn opened_thread(session: &ThreadHostSession) -> ThreadToolSummary {
    let trimmed = ThreadHostSession {
        session_id: session.session_id.clone(),
        durable_session_id: session.durable_session_id.clone(),
        cwd: session.cwd.clone(),
        name: session.name.clone(),
        status: session.status.clone(),
        created_at: session.created_at.clone(),
        updated_at: session.updated_at.clone(),
        ..Default::default()
    };
    summary(&trimmed, None)
}

#[derive(Debug, Clone)]
#[allow(clippy::large_enum_variant)]
pub enum SessionOpenOutcome {
    Ok(HostedThread),
    Error(Value),
}

pub trait SessionHost: Send + Sync {
    fn device(&self) -> Option<&str> {
        None
    }
    fn visible(&self) -> BoxFuture<Result<Vec<ThreadSummary>, String>>;
    fn open(
        &self,
        request: SessionOpenRequest,
        confirm_reach: bool,
    ) -> BoxFuture<Result<SessionOpenOutcome, String>>;
}
