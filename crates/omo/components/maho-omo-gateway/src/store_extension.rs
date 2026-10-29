//! Transactional store extension operations for gateway rules.
//!
//! Upstream counterpart: `store-extension/index.ts` and `store-extension/migrations.ts`.

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{Deserialize, Serialize};

use crate::migrations::{GATEWAY_RULES_EXTENSION_NAME, GATEWAY_RULES_MIGRATIONS};
use crate::rules_block::{escape_rule_text, render_operating_rules_block};

/// A target session in a rules commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesCommittedTarget {
    pub session_durable_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binding_id: Option<String>,
    /// Behavioral rule lines, or `None` to clear the session's rules.
    pub behavioral: Option<Vec<String>>,
}

/// Request parameters for `rulesCommitted`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RulesCommittedArgs {
    pub scope: String,
    pub version: String,
    #[serde(default = "default_now")]
    pub now: f64,
    pub targets: Vec<RulesCommittedTarget>,
}

fn default_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as f64)
        .unwrap_or(0.0)
}

/// Delivery enqueue outcome for a delivery.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DeliverySummary {
    Ok {
        delivery_id: String,
        deduplicated: bool,
    },
    Error {
        error: String,
    },
}

/// Per-session outcome of a rules commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedOutcome {
    pub session_durable_id: String,
    /// "updated", "unchanged", "removed", or "absent".
    pub outcome: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<DeliverySummary>,
}

/// Return value of `rulesCommitted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommittedResult {
    pub scope: String,
    pub version: String,
    pub outcomes: Vec<CommittedOutcome>,
}

/// Return value of `blockForSession`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRulesBlock {
    pub version: String,
    pub block: String,
}

/// A session entry returned by `sessionsWithRules`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionWithRule {
    pub session_durable_id: String,
    pub version: String,
}

/// Return value of `sessionsWithRules`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionsWithRulesResult {
    pub sessions: Vec<SessionWithRule>,
}

/// Typed delivery enqueue request matching upstream `tx.enqueue` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnqueueDeliveryRequest {
    pub binding_id: String,
    pub event_id: String,
    pub text: String,
    pub mode: String,
}

/// Typed delivery enqueue seam matching upstream `tx.enqueue`.
///
/// In upstream TypeScript, `rulesCommitted` executes inside an active store extension
/// transaction where `tx.enqueue` delegates to the store's relay/outbox implementation.
/// Because Thread SDK and OpenClaw external connectors are excluded from this crate's scope,
/// this typed seam receives each `EnqueueDeliveryRequest` within the active transaction
/// and returns the real delivery outcome or error.
pub trait GatewayDeliveryEnqueueSeam: Send + Sync {
    fn enqueue(
        &self,
        tx: &Transaction<'_>,
        request: &EnqueueDeliveryRequest,
        now: f64,
    ) -> Result<DeliverySummary, String>;
}

/// Apply initial migrations to a rusqlite Connection with idempotent migration bookkeeping.
/// Records applied version in `extension_schema` so restart on populated database does not
/// re-execute `CREATE TABLE` and cause `table already exists` errors.
/// Apply initial migrations to a rusqlite Connection with idempotent migration bookkeeping.
/// Records applied version in `extension_schema` (name, version, updated_at) in a single atomic transaction.
/// If `now` is provided, uses it as the `updated_at` timestamp; otherwise uses the current epoch millis.
pub fn apply_gateway_rules_migrations_with_now(
    conn: &mut Connection,
    now: f64,
) -> Result<(), rusqlite::Error> {
    let tx = conn.transaction()?;

    // 1. Ensure exact 3-column extension_schema bookkeeping table exists
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_schema (
            name TEXT PRIMARY KEY,
            version INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        )"#,
        [],
    )?;

    // 2. Check current applied migration version for GATEWAY_RULES_EXTENSION_NAME
    let current_version: Option<i64> = {
        let mut stmt = tx.prepare("SELECT version FROM extension_schema WHERE name = ?")?;
        stmt.query_row(params![GATEWAY_RULES_EXTENSION_NAME], |r| r.get(0))
            .optional()?
    };

    let start_version = current_version.unwrap_or(0);
    let total_migrations = GATEWAY_RULES_MIGRATIONS.len() as i64;

    // Upstream: if version > migrations.length, throw GatewaySchemaVersionError
    if start_version > total_migrations {
        return Err(rusqlite::Error::InvalidQuery);
    }

    let now_i64 = now as i64;

    if start_version == total_migrations {
        if current_version.is_none() {
            tx.execute(
                "INSERT INTO extension_schema (name, version, updated_at) VALUES (?, ?, ?)",
                params![GATEWAY_RULES_EXTENSION_NAME, total_migrations, now_i64],
            )?;
        }
        tx.commit()?;
        return Ok(());
    }

    // Apply remaining migrations sequentially
    for (idx, stage) in GATEWAY_RULES_MIGRATIONS.iter().enumerate().skip(start_version as usize) {
        for sql in *stage {
            tx.execute(sql, [])?;
        }
        let next_version = (idx + 1) as i64;
        tx.execute(
            r#"INSERT INTO extension_schema (name, version, updated_at) VALUES (?1, ?2, ?3)
               ON CONFLICT(name) DO UPDATE SET version = excluded.version, updated_at = excluded.updated_at"#,
            params![GATEWAY_RULES_EXTENSION_NAME, next_version, now_i64],
        )?;
    }

    tx.commit()?;
    Ok(())
}

/// Apply initial migrations using current system timestamp.
pub fn apply_gateway_rules_migrations(conn: &mut Connection) -> Result<(), rusqlite::Error> {
    apply_gateway_rules_migrations_with_now(conn, default_now())
}

/// Read block for session: `SELECT version, block FROM gateway_rules_blocks WHERE session_durable_id = ?`.
/// Empty session durable ID returns an error matching `requiredString` semantics.
pub fn block_for_session(
    conn: &Connection,
    session_durable_id: &str,
) -> Result<Option<SessionRulesBlock>, rusqlite::Error> {
    if session_durable_id.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "session_durable_id must be a non-empty string".into(),
        ));
    }
    let mut stmt = conn.prepare(
        "SELECT version, block FROM gateway_rules_blocks WHERE session_durable_id = ?",
    )?;
    let row = stmt
        .query_row(params![session_durable_id], |r| {
            Ok(SessionRulesBlock {
                version: r.get(0)?,
                block: r.get(1)?,
            })
        })
        .optional()?;
    Ok(row)
}

/// List sessions in a scope: `SELECT session_durable_id, version FROM gateway_rules_blocks WHERE scope = ? ORDER BY session_durable_id`.
pub fn sessions_with_rules(
    conn: &Connection,
    scope: &str,
) -> Result<SessionsWithRulesResult, rusqlite::Error> {
    if scope.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "scope must be a non-empty string".into(),
        ));
    }
    let mut stmt = conn.prepare(
        "SELECT session_durable_id, version FROM gateway_rules_blocks WHERE scope = ? ORDER BY session_durable_id",
    )?;
    let rows = stmt.query_map(params![scope], |r| {
        Ok(SessionWithRule {
            session_durable_id: r.get(0)?,
            version: r.get(1)?,
        })
    })?;

    let mut sessions = Vec::new();
    for row in rows {
        sessions.push(row?);
    }
    Ok(SessionsWithRulesResult { sessions })
}

/// Fan out one rules commit.
///
/// Upsert each affected session's rendered block when its version moved,
/// clear it when the target's behavioral is `None`, and append exactly one
/// `rules_changed` delivery per session and version via the enqueue seam.
pub fn rules_committed(
    conn: &mut Connection,
    args: &RulesCommittedArgs,
    enqueue_seam: &dyn GatewayDeliveryEnqueueSeam,
) -> Result<CommittedResult, rusqlite::Error> {
    if args.scope.is_empty() || args.version.is_empty() {
        return Err(rusqlite::Error::InvalidParameterName(
            "scope and version must be non-empty strings".into(),
        ));
    }

    if !args.now.is_finite() {
        return Err(rusqlite::Error::InvalidParameterName(
            "now must be a finite timestamp".into(),
        ));
    }

    let tx = conn.transaction()?;
    let mut outcomes = Vec::with_capacity(args.targets.len());

    for target in &args.targets {
        if target.session_durable_id.is_empty() {
            return Err(rusqlite::Error::InvalidParameterName(
                "session_durable_id must be a non-empty string".into(),
            ));
        }

        if let Some(behavioral) = &target.behavioral {
            let block = render_operating_rules_block(&args.version, behavioral);
            let written = tx.execute(
                r#"INSERT INTO gateway_rules_blocks (session_durable_id, scope, version, block, updated_at)
                   VALUES (?1, ?2, ?3, ?4, ?5)
                   ON CONFLICT(session_durable_id) DO UPDATE SET
                     scope = excluded.scope,
                     version = excluded.version,
                     block = excluded.block,
                     updated_at = excluded.updated_at
                   WHERE gateway_rules_blocks.version != excluded.version"#,
                params![
                    target.session_durable_id,
                    args.scope,
                    args.version,
                    block,
                    args.now as i64
                ],
            )?;

            let outcome_str = if written > 0 { "updated" } else { "unchanged" };

            let delivery_summary = if let Some(binding_id) = &target.binding_id {
                let event_id = format!(
                    "rules_changed:{}:{}:{}",
                    args.scope, args.version, target.session_durable_id
                );
                let escaped_scope = escape_rule_text(&args.scope).replace('"', "&quot;");
                let escaped_version = escape_rule_text(&args.version).replace('"', "&quot;");
                let text = format!(
                    r#"<operating-rules-changed scope="{escaped_scope}" version="{escaped_version}">
The operating rules for this gateway scope changed; this session's system prompt carries the new block from its next turn.
</operating-rules-changed>"#
                );

                let req = EnqueueDeliveryRequest {
                    binding_id: binding_id.clone(),
                    event_id,
                    text,
                    mode: "follow_up".to_string(),
                };

                let summary = match enqueue_seam.enqueue(&tx, &req, args.now) {
                    Ok(summary) => summary,
                    Err(e) => DeliverySummary::Error { error: e },
                };

                Some(summary)
            } else {
                None
            };

            outcomes.push(CommittedOutcome {
                session_durable_id: target.session_durable_id.clone(),
                outcome: outcome_str.to_string(),
                delivery: delivery_summary,
            });
        } else {
            // target.behavioral is None: clear the session's rules
            let removed = tx.execute(
                "DELETE FROM gateway_rules_blocks WHERE session_durable_id = ?",
                params![target.session_durable_id],
            )?;
            outcomes.push(CommittedOutcome {
                session_durable_id: target.session_durable_id.clone(),
                outcome: if removed > 0 { "removed" } else { "absent" }.to_string(),
                delivery: None,
            });
        }
    }

    tx.commit()?;

    Ok(CommittedResult {
        scope: args.scope.clone(),
        version: args.version.clone(),
        outcomes,
    })
}
