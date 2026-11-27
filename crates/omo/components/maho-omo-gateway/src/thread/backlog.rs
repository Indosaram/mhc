//! Target delivery backlog queries and per-lane capacity refusals (`backlog.ts`).

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::constants::{
    OPEN_STATES, STEER_RESERVE_BYTES, STEER_RESERVE_MESSAGES, TARGET_MAX_BYTES, TARGET_MAX_MESSAGES,
};
use super::errors::ThreadErrorCode;
use super::store_ops::{
    SqlAccess, SqlRow, StoreContext, StoreError, claim_fingerprint, row_i64, row_opt_i64, row_str,
    select_rows, v_i64, v_str,
};
use super::types::{GatewayDeliveryMode, StoreRefusal};
use rusqlite::types::Value as SqliteValue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BacklogLane {
    Steer,
    FollowUp,
}

impl BacklogLane {
    fn as_str(self) -> &'static str {
        match self {
            Self::Steer => "steer",
            Self::FollowUp => "follow_up",
        }
    }

    fn limits(self) -> (i64, usize) {
        match self {
            Self::Steer => (STEER_RESERVE_MESSAGES, STEER_RESERVE_BYTES),
            Self::FollowUp => (TARGET_MAX_MESSAGES, TARGET_MAX_BYTES),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetBacklog {
    pub held: i64,
    pub held_bytes: i64,
    pub held_by_session: i64,
    pub waiting_in_gateway: i64,
    pub oldest_at: Option<i64>,
    pub frees_at: Option<i64>,
}

fn backlog_of(found: Option<&SqlRow>) -> TargetBacklog {
    let held = found.map_or(0, |r| row_i64(r, "n"));
    let by_session = found.map_or(0, |r| row_i64(r, "by_session"));
    TargetBacklog {
        held,
        held_bytes: found.map_or(0, |r| row_i64(r, "b")),
        held_by_session: by_session,
        waiting_in_gateway: held - by_session,
        oldest_at: found.and_then(|r| row_opt_i64(r, "oldest")),
        frees_at: found.and_then(|r| row_opt_i64(r, "frees")),
    }
}

fn backlog_select_prefix() -> String {
    format!(
        "SELECT target_durable_id AS target, COUNT(*) AS n, COALESCE(SUM(bytes), 0) AS b, \
         COALESCE(SUM(CASE WHEN state = 'queued' THEN 0 ELSE 1 END), 0) AS by_session, \
         MIN(created_at) AS oldest, \
         MIN(CASE WHEN state = 'queued' THEN expires_at END) AS frees \
         FROM deliveries WHERE state IN {OPEN_STATES} AND NOT (state = 'queued' AND expires_at <= ?)"
    )
}

fn read_target_backlog(
    ctx: &StoreContext,
    target: &str,
    lane: BacklogLane,
    now: i64,
    dead_claims: &HashMap<String, String>,
) -> Result<TargetBacklog, StoreError> {
    let lane_filter = match lane {
        BacklogLane::Steer => "mode_requested = 'steer'",
        BacklogLane::FollowUp => "mode_requested <> 'steer'",
    };
    let sql = format!(
        "{} AND target_durable_id = ? AND {lane_filter}",
        backlog_select_prefix()
    );
    let found = ctx.one(&sql, &[v_i64(now), v_str(target)])?;
    let mut counted = backlog_of(found.as_ref());
    if dead_claims.is_empty() {
        return Ok(counted);
    }
    let mut dead = 0_i64;
    let mut dead_bytes = 0_i64;
    let where_clause =
        format!("target_durable_id = ? AND state IN ('admitting', 'admitted') AND {lane_filter}");
    for row in select_rows(ctx, &where_clause, &[v_str(target)], None)? {
        if dead_claims.get(&row.delivery_id).map(String::as_str)
            == Some(claim_fingerprint(&row).as_str())
        {
            dead += 1;
            dead_bytes += row.bytes;
        }
    }
    counted.held -= dead;
    counted.held_bytes -= dead_bytes;
    counted.held_by_session -= dead;
    Ok(counted)
}

/// Every listed target's undelivered backlog in one read; a target holding nothing is absent.
pub fn read_target_backlogs(
    ctx: &StoreContext,
    targets: &[String],
    now: i64,
) -> Result<BTreeMap<String, TargetBacklog>, StoreError> {
    if targets.is_empty() {
        return Ok(BTreeMap::new());
    }
    let placeholders = targets.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!(
        "{} AND target_durable_id IN ({placeholders}) GROUP BY target_durable_id",
        backlog_select_prefix()
    );
    let mut params: Vec<SqliteValue> = Vec::with_capacity(targets.len() + 1);
    params.push(v_i64(now));
    for t in targets {
        params.push(v_str(t));
    }
    let rows = ctx.all(&sql, &params)?;
    let mut out = BTreeMap::new();
    for row in rows {
        out.insert(row_str(&row, "target"), backlog_of(Some(&row)));
    }
    Ok(out)
}

pub struct BacklogCheck<'a> {
    pub target: &'a str,
    pub mode: GatewayDeliveryMode,
    pub bytes: usize,
    pub now: i64,
    pub dead_claims: &'a HashMap<String, String>,
    pub target_running: bool,
}

pub fn backlog_refusal(
    ctx: &StoreContext,
    check: &BacklogCheck<'_>,
) -> Result<Option<StoreRefusal>, StoreError> {
    let lane = if check.mode == GatewayDeliveryMode::Steer {
        BacklogLane::Steer
    } else {
        BacklogLane::FollowUp
    };
    let (max_messages, max_bytes) = lane.limits();
    let backlog = read_target_backlog(ctx, check.target, lane, check.now, check.dead_claims)?;
    if backlog.held < max_messages
        && (backlog.held == 0 || backlog.held_bytes + check.bytes as i64 <= max_bytes as i64)
    {
        return Ok(None);
    }
    let stall = if backlog.held_by_session >= backlog.waiting_in_gateway {
        "session_not_writing"
    } else {
        "session_not_draining"
    };
    let message = if stall == "session_not_writing" {
        format!(
            "The target's {} backlog is full: the session took {} of them into its own queue and has not written them, because it has not reached a turn boundary since.",
            lane.as_str(),
            backlog.held_by_session
        )
    } else {
        format!(
            "The target's {} backlog is full: {} of them wait in the gateway for the session's drain, which has not taken them.",
            lane.as_str(),
            backlog.waiting_in_gateway
        )
    };
    let oldest_age_ms = backlog
        .oldest_at
        .map_or(0, |oldest| 0_i64.max(check.now - oldest));
    Ok(Some(StoreRefusal {
        code: ThreadErrorCode::parse("queue_full")
            .unwrap_or_else(|| unreachable!("queue_full is a valid code")),
        message,
        details: Some(json!({
            "lane": lane.as_str(),
            "max_messages": max_messages,
            "max_bytes": max_bytes,
            "held": backlog.held,
            "held_bytes": backlog.held_bytes,
            "held_by_session": backlog.held_by_session,
            "waiting_in_gateway": backlog.waiting_in_gateway,
            "oldest_age_ms": oldest_age_ms,
            "reason": stall,
            "target_running": check.target_running,
            "frees_at": Value::from(backlog.frees_at),
        })),
    }))
}
