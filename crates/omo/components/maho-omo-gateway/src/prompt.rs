//! Per-turn rules injection via `before_agent_start`.
//!
//! Upstream counterpart: `prompt.ts`.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use rusqlite::Connection;

use crate::rules_block::compose_gateway_rules_block;
use crate::store_extension::{block_for_session, SessionRulesBlock};

/// Abstraction over the gateway store for prompt rules lookup.
pub trait GatewayRulesStore: Send + Sync {
    fn block_for_session(&self, session_durable_id: &str) -> Result<Option<SessionRulesBlock>, String>;
}

/// A real rusqlite-backed store implementing `GatewayRulesStore`.
pub struct SqliteGatewayRulesStore {
    conn: std::sync::Mutex<Connection>,
}

impl SqliteGatewayRulesStore {
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: std::sync::Mutex::new(conn),
        }
    }
}

impl GatewayRulesStore for SqliteGatewayRulesStore {
    fn block_for_session(&self, session_durable_id: &str) -> Result<Option<SessionRulesBlock>, String> {
        let conn = self.conn.lock().map_err(|e| format!("Mutex poisoned: {e}"))?;
        block_for_session(&conn, session_durable_id).map_err(|e| e.to_string())
    }
}

pub type EnsureStoreFn = Arc<dyn Fn() -> Result<Option<Arc<dyn GatewayRulesStore>>, String> + Send + Sync>;
pub type LookupErrorFn = Arc<dyn Fn(&str) + Send + Sync>;

/// Options for configuring the prompt handler.
pub struct GatewayRulesPromptOptions {
    /// Lazily connects the gateway store on first need: a session without the gateway config,
    /// or a host without the store database, returns `None` and this handler is a pass-through.
    pub ensure_store: EnsureStoreFn,
    pub on_lookup_error: Option<LookupErrorFn>,
}

/// Result of evaluating rules injection on a system prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRulesPromptResult {
    pub system_prompt: String,
}

/// Handler for `before_agent_start` prompt injection.
pub struct GatewayRulesPromptHandler {
    options: GatewayRulesPromptOptions,
    lookup_warned: AtomicBool,
}

impl GatewayRulesPromptHandler {
    pub fn new(options: GatewayRulesPromptOptions) -> Self {
        Self {
            options,
            lookup_warned: AtomicBool::new(false),
        }
    }

    /// Process a `before_agent_start` prompt evaluation for a given session.
    ///
    /// Returns `None` if no rules are committed for this session, store is unavailable,
    /// or an error occurred (which logs a one-time warning).
    pub fn handle(
        &self,
        system_prompt: &str,
        session_id: &str,
    ) -> Option<GatewayRulesPromptResult> {
        if session_id.is_empty() {
            return None;
        }

        let store = match (self.options.ensure_store)() {
            Ok(Some(store)) => store,
            Ok(None) => return None,
            Err(err) => {
                self.warn_lookup_error(&err);
                return None;
            }
        };

        match store.block_for_session(session_id) {
            Ok(Some(row)) => Some(GatewayRulesPromptResult {
                system_prompt: compose_gateway_rules_block(system_prompt, &row.block),
            }),
            Ok(None) => None,
            Err(err) => {
                self.warn_lookup_error(&format!("gateway rules lookup failed: {err}"));
                None
            }
        }
    }

    fn warn_lookup_error(&self, message: &str) {
        if !self.lookup_warned.swap(true, Ordering::Relaxed)
            && let Some(on_error) = &self.options.on_lookup_error
        {
            on_error(message);
        }
    }
}
