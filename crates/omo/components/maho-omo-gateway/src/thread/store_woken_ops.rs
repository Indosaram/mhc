//! Woken-session lease management, session wait state, intake marks, and remote intake
//! (`store-worker.ts` woken/intake operations, extracted to keep `store_ops.rs` lean).

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};

use crate::paths::gateway_wake_marker_path;

use super::constants::{OPEN_STATES, WAKE_LEASE_MS, is_durable_id};
use super::errors::ThreadErrorCode;
use super::session_wait_types::{
    DrainedOutcome, IntakeMark, IntakePair, OpenIntakeSession, QueuedWaitKind, QueuedWaitingOn,
    SessionWaitKind, SessionWaitState, SessionWork, WriteSessionWaitRequest, wait_blocks_delivery,
};
use super::store_ops::{
    IntakeFields, SqlAccess, SqlRow, StoreContext, StoreError, args_hash, claimant_is_dead,
    delivery_view, new_id, row_i64, row_opt_i64, row_opt_str, row_str, transaction, v_i64,
    v_opt_i64, v_opt_str, v_str,
};
use super::types::{
    DeliveryRow, EnqueueOutcome, EnqueueRequest, EnvelopeOrigin, GatewayDeliveryMode,
    IntakeOutcome, IntakeRequest, ProcessIdentity, Provenance, ReceiptRef, StoreRefusal,
};
use super::woken_types::{
    WakeRequestOutcome, WakeRetryOutcome, WakeStartOutcome, WakeTakeOverOutcome, WokenSessionRow,
    WokenSessionState, WokenStateUpdate,
};

const WAKE_RETRY_DELAY_MS: i64 = 10_000;
const WAKE_MAX_ATTEMPTS: i64 = 3;

const WOKEN_COLUMNS: &[&str] = &[
    "durable_id",
    "session_file",
    "host_instance",
    "state",
    "requested_at",
    "opened_at",
    "first_waiting_at",
    "lease_owner",
    "lease_expires_at",
    "generation",
    "attempts",
    "next_attempt_at",
];

fn store_refusal(code: &str, message: impl Into<String>, details: Option<Value>) -> StoreRefusal {
    StoreRefusal {
        code: ThreadErrorCode::parse(code)
            .unwrap_or_else(|| panic!("unknown thread error code: {code}")),
        message: message.into(),
        details,
    }
}

fn woken_row_from(record: &SqlRow) -> WokenSessionRow {
    let state =
        WokenSessionState::parse(&row_str(record, "state")).unwrap_or(WokenSessionState::Requested);
    let lease_owner =
        row_opt_str(record, "lease_owner").and_then(|json| serde_json::from_str(&json).ok());
    WokenSessionRow {
        durable_id: row_str(record, "durable_id"),
        session_file: row_str(record, "session_file"),
        host_instance: row_opt_str(record, "host_instance"),
        state,
        requested_at: row_i64(record, "requested_at"),
        opened_at: row_opt_i64(record, "opened_at"),
        first_waiting_at: row_opt_i64(record, "first_waiting_at"),
        lease_owner,
        lease_expires_at: row_opt_i64(record, "lease_expires_at"),
        generation: row_i64(record, "generation"),
        attempts: row_i64(record, "attempts"),
        next_attempt_at: row_opt_i64(record, "next_attempt_at"),
    }
}

fn select_woken_row(
    ctx: &StoreContext,
    durable_id: &str,
) -> Result<Option<WokenSessionRow>, StoreError> {
    let sql = format!(
        "SELECT {} FROM woken_sessions WHERE durable_id = ?",
        WOKEN_COLUMNS.join(", ")
    );
    Ok(ctx
        .one(&sql, &[v_str(durable_id)])?
        .as_ref()
        .map(woken_row_from))
}

fn require_woken_row(ctx: &StoreContext, durable_id: &str) -> Result<WokenSessionRow, StoreError> {
    select_woken_row(ctx, durable_id)?.ok_or_else(|| {
        StoreError::Message(format!(
            "woken session {durable_id} vanished inside its own transaction"
        ))
    })
}

fn touch_wake_marker(ctx: &StoreContext, durable_id: &str, now: i64) -> Result<(), StoreError> {
    let marker = gateway_wake_marker_path(Path::new(&ctx.config.agent_dir));
    if let Some(parent) = marker.parent() {
        std::fs::create_dir_all(parent).map_err(|e| StoreError::Message(e.to_string()))?;
    }
    let temporary = marker.with_file_name(format!(
        "wake.marker.{}.{}.tmp",
        std::process::id(),
        new_id("tmp")
    ));
    let payload = json!({ "durable_id": durable_id, "written_at": now }).to_string();
    std::fs::write(&temporary, payload.as_bytes())
        .map_err(|e| StoreError::Message(e.to_string()))?;
    std::fs::rename(&temporary, &marker).map_err(|e| StoreError::Message(e.to_string()))?;
    Ok(())
}

pub fn woken_request(
    ctx: &StoreContext,
    durable_id: &str,
    session_file: &str,
    now: i64,
) -> Result<WakeRequestOutcome, StoreError> {
    transaction(ctx, "woken_request", || {
        let inserted = ctx.run(
            "INSERT OR IGNORE INTO woken_sessions (durable_id, session_file, state, requested_at) VALUES (?, ?, 'requested', ?)",
            &[v_str(durable_id), v_str(session_file), v_i64(now)],
        )?;
        touch_wake_marker(ctx, durable_id, now)?;
        Ok(WakeRequestOutcome {
            row: require_woken_row(ctx, durable_id)?,
            created: inserted == 1,
        })
    })
}

pub fn woken_start(
    ctx: &StoreContext,
    durable_id: &str,
    now: i64,
) -> Result<WakeStartOutcome, StoreError> {
    transaction(ctx, "woken_start", || {
        let Some(row) = select_woken_row(ctx, durable_id)? else {
            return Ok(WakeStartOutcome::NotFound);
        };
        if row.state != WokenSessionState::Requested {
            return Ok(WakeStartOutcome::InProgress { row });
        }
        if let Some(next_at) = row.next_attempt_at
            && next_at > now
        {
            return Ok(WakeStartOutcome::NotYet {
                next_attempt_at: next_at,
            });
        }
        let owner_json = serde_json::to_string(&ctx.self_identity).unwrap_or_default();
        ctx.run(
            "UPDATE woken_sessions SET state = 'opening', lease_owner = ?, lease_expires_at = ?, generation = generation + 1, next_attempt_at = NULL WHERE durable_id = ?",
            &[v_str(owner_json), v_i64(now + WAKE_LEASE_MS), v_str(durable_id)],
        )?;
        Ok(WakeStartOutcome::Started {
            row: require_woken_row(ctx, durable_id)?,
        })
    })
}

pub fn woken_renew(
    ctx: &StoreContext,
    durable_id: &str,
    generation: i64,
    now: i64,
) -> Result<Option<WokenSessionRow>, StoreError> {
    transaction(ctx, "woken_renew", || {
        let updated = ctx.run(
            "UPDATE woken_sessions SET lease_expires_at = ? WHERE durable_id = ? AND generation = ?",
            &[
                v_i64(now + WAKE_LEASE_MS),
                v_str(durable_id),
                v_i64(generation),
            ],
        )?;
        if updated == 1 {
            Ok(Some(require_woken_row(ctx, durable_id)?))
        } else {
            Ok(None)
        }
    })
}

pub fn woken_take_over(
    ctx: &StoreContext,
    durable_id: &str,
    now: i64,
) -> Result<WakeTakeOverOutcome, StoreError> {
    let Some(seen) = select_woken_row(ctx, durable_id)? else {
        return Ok(WakeTakeOverOutcome::NotFound);
    };
    let (Some(lease_owner), Some(lease_expires_at)) = (&seen.lease_owner, seen.lease_expires_at)
    else {
        return Ok(WakeTakeOverOutcome::NotLeased { row: seen });
    };
    if lease_expires_at > now {
        return Ok(WakeTakeOverOutcome::LeaseLive { row: seen });
    }
    let previous_owner_alive = !claimant_is_dead(lease_owner);
    transaction(ctx, "woken_take_over", || {
        let Some(current) = select_woken_row(ctx, durable_id)? else {
            return Ok(WakeTakeOverOutcome::NotFound);
        };
        if current.generation != seen.generation {
            return Ok(WakeTakeOverOutcome::LostLease);
        }
        if let Some(exp) = current.lease_expires_at
            && exp > now
        {
            return Ok(WakeTakeOverOutcome::LeaseLive { row: current });
        }
        let owner_json = serde_json::to_string(&ctx.self_identity).unwrap_or_default();
        ctx.run(
            "UPDATE woken_sessions SET lease_owner = ?, lease_expires_at = ?, generation = generation + 1 WHERE durable_id = ? AND generation = ?",
            &[
                v_str(owner_json),
                v_i64(now + WAKE_LEASE_MS),
                v_str(durable_id),
                v_i64(current.generation),
            ],
        )?;
        Ok(WakeTakeOverOutcome::TakenOver {
            row: require_woken_row(ctx, durable_id)?,
            previous_owner_alive,
        })
    })
}

pub fn woken_set_state(
    ctx: &StoreContext,
    update: &WokenStateUpdate,
    now: i64,
) -> Result<Option<WokenSessionRow>, StoreError> {
    if update.state == WokenSessionState::Requested {
        return Err(StoreError::Message(
            "woken_set_state cannot transition back to requested".to_owned(),
        ));
    }
    transaction(ctx, "woken_set_state", || {
        let opened = (update.state == WokenSessionState::Open).then_some(now);
        let updated = ctx.run(
            "UPDATE woken_sessions SET state = ?, opened_at = COALESCE(opened_at, ?), host_instance = COALESCE(?, host_instance) WHERE durable_id = ? AND generation = ?",
            &[
                v_str(update.state.as_str()),
                v_opt_i64(opened),
                v_opt_str(update.host_instance.as_deref()),
                v_str(&update.durable_id),
                v_i64(update.generation),
            ],
        )?;
        if updated == 1 {
            Ok(Some(require_woken_row(ctx, &update.durable_id)?))
        } else {
            Ok(None)
        }
    })
}

pub fn woken_record_waiting(
    ctx: &StoreContext,
    durable_id: &str,
    generation: i64,
    now: i64,
) -> Result<Option<WokenSessionRow>, StoreError> {
    transaction(ctx, "woken_record_waiting", || {
        let updated = ctx.run(
            "UPDATE woken_sessions SET first_waiting_at = COALESCE(first_waiting_at, ?) WHERE durable_id = ? AND generation = ?",
            &[v_i64(now), v_str(durable_id), v_i64(generation)],
        )?;
        if updated == 1 {
            Ok(Some(require_woken_row(ctx, durable_id)?))
        } else {
            Ok(None)
        }
    })
}

pub fn woken_retry_later(
    ctx: &StoreContext,
    durable_id: &str,
    generation: i64,
    now: i64,
) -> Result<WakeRetryOutcome, StoreError> {
    transaction(ctx, "woken_retry_later", || {
        let Some(row) = select_woken_row(ctx, durable_id)? else {
            return Ok(WakeRetryOutcome::LostLease);
        };
        if row.generation != generation {
            return Ok(WakeRetryOutcome::LostLease);
        }
        let attempts = row.attempts + 1;
        if attempts >= WAKE_MAX_ATTEMPTS {
            ctx.run(
                "DELETE FROM woken_sessions WHERE durable_id = ? AND generation = ?",
                &[v_str(durable_id), v_i64(generation)],
            )?;
            touch_wake_marker(ctx, durable_id, now)?;
            return Ok(WakeRetryOutcome::Failed { attempts });
        }
        ctx.run(
            "UPDATE woken_sessions SET state = 'requested', lease_owner = NULL, lease_expires_at = NULL, attempts = ?, next_attempt_at = ? WHERE durable_id = ? AND generation = ?",
            &[
                v_i64(attempts),
                v_i64(now + WAKE_RETRY_DELAY_MS),
                v_str(durable_id),
                v_i64(generation),
            ],
        )?;
        Ok(WakeRetryOutcome::Deferred {
            row: require_woken_row(ctx, durable_id)?,
        })
    })
}

pub fn woken_clear_waiting(
    ctx: &StoreContext,
    durable_id: &str,
    generation: i64,
) -> Result<bool, StoreError> {
    transaction(ctx, "woken_clear_waiting", || {
        let updated = ctx.run(
            "UPDATE woken_sessions SET first_waiting_at = NULL WHERE durable_id = ? AND generation = ?",
            &[v_str(durable_id), v_i64(generation)],
        )?;
        Ok(updated == 1)
    })
}

pub fn woken_end(
    ctx: &StoreContext,
    durable_id: &str,
    generation: i64,
    now: i64,
) -> Result<bool, StoreError> {
    transaction(ctx, "woken_end", || {
        let deleted = ctx.run(
            "DELETE FROM woken_sessions WHERE durable_id = ? AND generation = ?",
            &[v_str(durable_id), v_i64(generation)],
        )?;
        if deleted != 1 {
            return Ok(false);
        }
        touch_wake_marker(ctx, durable_id, now)?;
        Ok(true)
    })
}

pub fn woken_gated(ctx: &StoreContext, durable_id: &str, now: i64) -> Result<bool, StoreError> {
    let Some(row) = select_woken_row(ctx, durable_id)? else {
        return Ok(false);
    };
    Ok(
        row.state == WokenSessionState::Opening
            && row.lease_expires_at.is_some_and(|exp| exp > now),
    )
}

pub fn woken_list(ctx: &StoreContext) -> Result<Vec<WokenSessionRow>, StoreError> {
    let sql = format!(
        "SELECT {} FROM woken_sessions ORDER BY requested_at, durable_id",
        WOKEN_COLUMNS.join(", ")
    );
    Ok(ctx.all(&sql, &[])?.iter().map(woken_row_from).collect())
}

pub fn session_wait_read(
    ctx: &StoreContext,
    durable_id: &str,
) -> Result<Option<SessionWaitState>, StoreError> {
    let Some(found) = ctx.one(
        "SELECT durable_id, wait, hold_kinds, file_size, written_at FROM session_wait WHERE durable_id = ?",
        &[v_str(durable_id)],
    )?
    else {
        return Ok(None);
    };
    let hold_kinds: Vec<String> =
        serde_json::from_str(&row_str(&found, "hold_kinds")).unwrap_or_default();
    let wait = SessionWaitKind::parse(&row_str(&found, "wait")).unwrap_or(SessionWaitKind::Waiting);
    Ok(Some(SessionWaitState {
        durable_id: row_str(&found, "durable_id"),
        wait,
        hold_kinds,
        file_size: row_i64(&found, "file_size"),
        written_at: row_i64(&found, "written_at"),
    }))
}

pub fn session_wait_write(
    ctx: &StoreContext,
    request: &WriteSessionWaitRequest,
    now: i64,
) -> Result<(), StoreError> {
    let kinds_json = serde_json::to_string(&request.hold_kinds).unwrap_or_else(|_| "[]".to_owned());
    transaction(ctx, "wait_write", || {
        ctx.run(
            "INSERT INTO session_wait (durable_id, wait, hold_kinds, file_size, written_at) VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT (durable_id) DO UPDATE SET wait = excluded.wait, hold_kinds = excluded.hold_kinds, \
               file_size = excluded.file_size, written_at = excluded.written_at",
            &[
                v_str(&request.durable_id),
                v_str(request.wait.as_str()),
                v_str(kinds_json),
                v_i64(request.file_size),
                v_i64(now),
            ],
        )?;
        Ok(())
    })
}

pub fn session_wait_clear(ctx: &StoreContext, durable_id: &str) -> Result<(), StoreError> {
    transaction(ctx, "wait_clear", || {
        ctx.run(
            "DELETE FROM session_wait WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?;
        Ok(())
    })
}

pub fn session_waiting_on(
    ctx: &StoreContext,
    durable_id: &str,
) -> Result<Option<QueuedWaitingOn>, StoreError> {
    if let Some(wait) = session_wait_read(ctx, durable_id)?
        && wait_blocks_delivery(&wait)
    {
        return Ok(Some(QueuedWaitingOn {
            kind: if wait.wait == SessionWaitKind::Held {
                QueuedWaitKind::Hold
            } else {
                QueuedWaitKind::Approval
            },
        }));
    }
    let woken = ctx
        .one(
            "SELECT COUNT(*) AS n FROM woken_sessions WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?
        .map_or(0, |r| row_i64(&r, "n"));
    Ok((woken > 0).then_some(QueuedWaitingOn {
        kind: QueuedWaitKind::Wake,
    }))
}

pub fn count_wake(
    ctx: &StoreContext,
    durable_id: &str,
    human_input_revision: Option<i64>,
) -> Result<i64, StoreError> {
    transaction(ctx, "wake_count", || {
        let found = ctx.one(
            "SELECT count, human_input_revision FROM wake_counts WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?;
        let stored = found
            .as_ref()
            .and_then(|r| row_opt_i64(r, "human_input_revision"));
        let moved = found.is_none() || human_input_revision.is_some_and(|rev| stored != Some(rev));
        let count = if moved {
            1
        } else {
            found.as_ref().map_or(0, |r| row_i64(r, "count")) + 1
        };
        let next_rev = human_input_revision.or(stored);
        ctx.run(
            "INSERT INTO wake_counts (durable_id, count, human_input_revision) VALUES (?, ?, ?) \
             ON CONFLICT (durable_id) DO UPDATE SET count = excluded.count, human_input_revision = excluded.human_input_revision",
            &[v_str(durable_id), v_i64(count), v_opt_i64(next_rev)],
        )?;
        Ok(count)
    })
}

pub fn wake_count(ctx: &StoreContext, durable_id: &str) -> Result<i64, StoreError> {
    Ok(ctx
        .one(
            "SELECT count FROM wake_counts WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?
        .map_or(0, |r| row_i64(&r, "count")))
}

pub fn reset_wake_count(ctx: &StoreContext, durable_id: &str) -> Result<(), StoreError> {
    transaction(ctx, "wake_count_reset", || {
        ctx.run(
            "DELETE FROM wake_counts WHERE durable_id = ?",
            &[v_str(durable_id)],
        )?;
        Ok(())
    })
}

pub fn session_work_of(ctx: &StoreContext, durable_id: &str) -> Result<SessionWork, StoreError> {
    let count = |state: &str| -> Result<i64, StoreError> {
        Ok(ctx
            .one(
                "SELECT COUNT(*) AS n FROM deliveries WHERE target_durable_id = ? AND state = ?",
                &[v_str(durable_id), v_str(state)],
            )?
            .map_or(0, |r| row_i64(&r, "n")))
    };
    Ok(SessionWork {
        queued: count("queued")?,
        claimed: count("admitting")?,
        unapplied: count("admitted")?,
    })
}

pub fn record_intake_command(
    ctx: &StoreContext,
    sender_device: &str,
    session_id: &str,
    command_id: &str,
    ledger_id: Option<&str>,
    now: i64,
) -> Result<(), StoreError> {
    transaction(ctx, "intake_mark", || {
        if let Some(lid) = ledger_id {
            ctx.run(
                "INSERT OR IGNORE INTO intake_commands (ledger_id, command_id) VALUES (?, ?)",
                &[v_str(lid), v_str(command_id)],
            )?;
        }
        ctx.run(
            "INSERT INTO intake_marks (sender_device, session_id, last_command_id, updated_at) VALUES (?, ?, ?, ?) \
             ON CONFLICT (sender_device, session_id) DO UPDATE SET last_command_id = excluded.last_command_id, \
               updated_at = excluded.updated_at",
            &[
                v_str(sender_device),
                v_str(session_id),
                v_str(command_id),
                v_i64(now),
            ],
        )?;
        Ok(())
    })
}

fn open_mark_rows(ctx: &StoreContext, pair: &IntakePair) -> Result<i64, StoreError> {
    let prefix = format!("device:{}/", pair.sender_device);
    let sql = format!(
        "SELECT COUNT(*) AS n FROM deliveries \
         WHERE target_durable_id = ? AND ledger_id IS NOT NULL AND substr(ledger_id, 1, length(?)) = ? AND state IN {OPEN_STATES}"
    );
    Ok(ctx
        .one(
            &sql,
            &[v_str(&pair.session_id), v_str(&prefix), v_str(&prefix)],
        )?
        .map_or(0, |r| row_i64(&r, "n")))
}

pub fn marked_pairs(ctx: &StoreContext) -> Result<Vec<IntakePair>, StoreError> {
    Ok(ctx
        .all(
            "SELECT sender_device, session_id FROM intake_marks ORDER BY sender_device, session_id",
            &[],
        )?
        .into_iter()
        .map(|row| IntakePair {
            sender_device: row_str(&row, "sender_device"),
            session_id: row_str(&row, "session_id"),
        })
        .collect())
}

pub fn pending_marks(ctx: &StoreContext) -> Result<Vec<IntakeMark>, StoreError> {
    let rows = ctx.all(
        "SELECT sender_device, session_id, last_command_id FROM intake_marks ORDER BY sender_device, session_id",
        &[],
    )?;
    let mut out = Vec::new();
    for row in rows {
        let pair = IntakePair {
            sender_device: row_str(&row, "sender_device"),
            session_id: row_str(&row, "session_id"),
        };
        if open_mark_rows(ctx, &pair)? > 0 {
            out.push(IntakeMark {
                sender_device: pair.sender_device,
                session_id: pair.session_id,
                last_command_id: row_str(&row, "last_command_id"),
            });
        }
    }
    Ok(out)
}

pub fn drained_mark(ctx: &StoreContext, pair: &IntakePair) -> Result<DrainedOutcome, StoreError> {
    transaction(ctx, "intake_drained", || {
        if open_mark_rows(ctx, pair)? > 0 {
            return Ok(DrainedOutcome::NotDrained);
        }
        let Some(mark) = ctx.one(
            "SELECT last_command_id FROM intake_marks WHERE sender_device = ? AND session_id = ?",
            &[v_str(&pair.sender_device), v_str(&pair.session_id)],
        )?
        else {
            return Ok(DrainedOutcome::NotDrained);
        };
        ctx.run(
            "DELETE FROM intake_marks WHERE sender_device = ? AND session_id = ?",
            &[v_str(&pair.sender_device), v_str(&pair.session_id)],
        )?;
        Ok(DrainedOutcome::Drained {
            last_command_id: row_str(&mark, "last_command_id"),
        })
    })
}

pub fn open_intake_sessions(ctx: &StoreContext) -> Result<Vec<OpenIntakeSession>, StoreError> {
    let sql = format!(
        "SELECT target_durable_id, json_extract(envelope, '$.provenance.device') AS device, state, admitted_by \
         FROM deliveries WHERE ledger_id IS NOT NULL AND state IN {OPEN_STATES} ORDER BY target_durable_id"
    );
    let rows = ctx.all(&sql, &[])?;
    let mut sessions: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for row in rows {
        let state = row_str(&row, "state");
        if state != "queued" {
            let claimant = row_opt_str(&row, "admitted_by")
                .and_then(|json| serde_json::from_str::<ProcessIdentity>(&json).ok());
            if claimant.as_ref().is_some_and(|c| !claimant_is_dead(c)) {
                continue;
            }
        }
        let entry = sessions
            .entry(row_str(&row, "target_durable_id"))
            .or_default();
        if let Some(device) = row_opt_str(&row, "device") {
            entry.insert(device);
        }
    }
    let local_rows = ctx.all(
        "SELECT DISTINCT target_durable_id FROM deliveries WHERE ledger_id IS NULL AND state = 'queued'",
        &[],
    )?;
    let mut local_ids = BTreeSet::new();
    for row in local_rows {
        let id = row_str(&row, "target_durable_id");
        sessions.entry(id.clone()).or_default();
        local_ids.insert(id);
    }
    Ok(sessions
        .into_iter()
        .map(|(durable_id, devices)| OpenIntakeSession {
            local: local_ids.contains(&durable_id),
            durable_id,
            devices: devices.into_iter().collect(),
        })
        .collect())
}

pub fn command_for_row(ctx: &StoreContext, ledger_id: &str) -> Result<Option<String>, StoreError> {
    Ok(ctx
        .one(
            "SELECT command_id FROM intake_commands WHERE ledger_id = ?",
            &[v_str(ledger_id)],
        )?
        .map(|r| row_str(&r, "command_id")))
}

pub fn open_for_mark_command(
    ctx: &StoreContext,
    sender_device: &str,
    command_id: &str,
) -> Result<bool, StoreError> {
    let prefix = format!("device:{sender_device}/");
    let sql = format!(
        "SELECT COUNT(*) AS n FROM intake_commands c JOIN deliveries d ON d.ledger_id = c.ledger_id \
         WHERE c.command_id = ? AND substr(c.ledger_id, 1, length(?)) = ? AND d.state IN {OPEN_STATES}"
    );
    Ok(ctx
        .one(&sql, &[v_str(command_id), v_str(&prefix), v_str(&prefix)])?
        .map_or(0, |r| row_i64(&r, "n"))
        > 0)
}

fn answer_from_ledger_row(
    row: &DeliveryRow,
    queue_position: i64,
    ledger_id: &str,
    record: &super::types::IntakeRecord,
) -> IntakeOutcome {
    let sender_session = (record.sender.kind == "agent")
        .then_some(record.sender.session_id.as_deref())
        .flatten();
    let same_content = row.target_durable_id == record.session_id
        && row.verbatim_text.as_deref() == Some(record.text.as_str())
        && row.verbatim_display_text.as_deref() == Some(record.display_text.as_str())
        && row.hop == record.chain_count
        && row.envelope.origin.session_id() == sender_session;
    if !same_content {
        return IntakeOutcome::Refused(store_refusal(
            "idempotency_conflict",
            "This delivery id was already taken in with other content.",
            Some(json!({ "ledger_id": ledger_id })),
        ));
    }
    IntakeOutcome::Queued {
        delivery_id: record.delivery_id.clone(),
        row_id: row.delivery_id.clone(),
        ledger_id: ledger_id.to_owned(),
        queue_position,
        deduplicated: true,
    }
}

pub fn intake(ctx: &StoreContext, request: &IntakeRequest) -> Result<IntakeOutcome, StoreError> {
    let record = &request.record;
    let device = &request.attested_device;
    if !is_durable_id(device)
        || !is_durable_id(&record.delivery_id)
        || !is_durable_id(&record.session_id)
        || record.chain_count < 1
    {
        return Ok(IntakeOutcome::Refused(store_refusal(
            "invalid_arguments",
            "The intake record is malformed.",
            None,
        )));
    }
    let sender_session = if record.sender.kind == "agent" {
        record.sender.session_id.as_deref()
    } else {
        None
    };
    let Some(sender_session) = sender_session.filter(|s| is_durable_id(s)) else {
        return Ok(IntakeOutcome::Refused(store_refusal(
            "unsupported",
            "Only an agent sender can reach a session that is not running.",
            Some(json!({ "reason": "sender_not_allowed" })),
        )));
    };
    if record.deliver_as != "followUp" {
        return Ok(IntakeOutcome::Refused(store_refusal(
            "turn_conflict",
            "A session that is not running has no turn to steer.",
            Some(json!({ "target": record.session_id })),
        )));
    }
    if request.target_session_kind != "interactive" {
        return Ok(IntakeOutcome::Refused(store_refusal(
            "not_resumable",
            "A child or worker session is not started for a delivery.",
            Some(json!({ "reason": "not_wakeable" })),
        )));
    }
    if let Some(limit) = request.chain_count_limit
        && record.chain_count > limit
    {
        return Ok(IntakeOutcome::Refused(store_refusal(
            "unsupported",
            "The delivery's chain is over this machine's chain limit.",
            Some(json!({
                "reason": "chain_count_limit",
                "chain_count": record.chain_count,
                "limit": limit,
            })),
        )));
    }
    let ledger_id = format!("device:{device}/{}", record.delivery_id);
    let principal = format!("device:{device}");
    let name = record
        .sender
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let enqueue_req = EnqueueRequest {
        provenance: Provenance {
            workspace: request.provenance.workspace.clone(),
            project: request.provenance.project.clone(),
            lane: request.provenance.lane.clone(),
            session: request.provenance.session.clone(),
            device: Some(device.clone()),
            device_absent: None,
        },
        now: request.now,
        delivery_id: new_id("delivery"),
        target_durable_id: record.session_id.clone(),
        sender_principal: principal.clone(),
        rate_principal: Some(principal.clone()),
        sender_node: format!("{principal}#session:{sender_session}"),
        sender_turn: None,
        cause_delivery_id: None,
        inbound_hop: Some(record.chain_count - 1),
        authority: None,
        claimed_root_id: None,
        origin: EnvelopeOrigin::Session {
            session: sender_session.to_owned(),
        },
        actor: name.unwrap_or(sender_session).to_owned(),
        body: record.text.clone(),
        mode: GatewayDeliveryMode::FollowUp,
        expected_turn_id: None,
        binding: None,
        receipt: ReceiptRef {
            idempotency_key: record.delivery_id.clone(),
            args_hash: args_hash(&json!({
                "target": record.session_id,
                "text": record.text,
                "display_text": record.display_text,
                "deliver_as": record.deliver_as,
                "chain_count": record.chain_count,
            })),
        },
        endpoint_kind: None,
        collapse_key: None,
    };
    let ledger_id_clone = ledger_id.clone();
    let delivery_id_copy = record.delivery_id.clone();
    let answer_fn = |row: &DeliveryRow, pos: i64| -> Value {
        json!({
            "kind": "queued",
            "delivery_id": delivery_id_copy,
            "row_id": row.delivery_id,
            "ledger_id": ledger_id_clone,
            "queue_position": pos,
        })
    };
    let intake_fields = IntakeFields {
        ledger_id: &ledger_id,
        verbatim_text: &record.text,
        verbatim_display_text: &record.display_text,
        answer: &answer_fn,
        existing: &|row, pos| {
            let outcome = answer_from_ledger_row(row, pos, &ledger_id, record);
            EnqueueOutcome::Replay {
                result: serde_json::to_value(outcome).unwrap_or(Value::Null),
            }
        },
    };
    let outcome =
        match super::store_ops::enqueue_with_intake(ctx, &enqueue_req, Some(intake_fields)) {
            Ok(out) => out,
            Err(StoreError::Sql(err))
                if err
                    .to_string()
                    .contains("UNIQUE constraint failed: deliveries.ledger_id") =>
            {
                let held = ctx.one(
                    "SELECT delivery_id FROM deliveries WHERE ledger_id = ?",
                    &[v_str(&ledger_id)],
                )?;
                if let Some(id) = held.as_ref().map(|r| row_str(r, "delivery_id"))
                    && let Some(view) = delivery_view(ctx, &id)?
                {
                    return Ok(answer_from_ledger_row(
                        &view.row,
                        view.queue_position,
                        &ledger_id,
                        record,
                    ));
                }
                return Err(StoreError::Sql(err));
            }
            Err(err) => return Err(err),
        };
    match outcome {
        EnqueueOutcome::Inserted {
            row,
            queue_position,
        } => Ok(IntakeOutcome::Queued {
            delivery_id: record.delivery_id.clone(),
            row_id: row.delivery_id,
            ledger_id,
            queue_position,
            deduplicated: false,
        }),
        EnqueueOutcome::Collapsed {
            row,
            queue_position,
        } => Ok(IntakeOutcome::Queued {
            delivery_id: record.delivery_id.clone(),
            row_id: row.delivery_id,
            ledger_id,
            queue_position,
            deduplicated: true,
        }),
        EnqueueOutcome::Replay { result } => {
            if let Ok(mut parsed) = serde_json::from_value::<IntakeOutcome>(result.clone()) {
                if let IntakeOutcome::Queued { deduplicated, .. } = &mut parsed {
                    *deduplicated = true;
                }
                Ok(parsed)
            } else {
                Ok(IntakeOutcome::Busy)
            }
        }
        EnqueueOutcome::Refused {
            code,
            message,
            details,
        } => Ok(IntakeOutcome::Refused(StoreRefusal {
            code,
            message,
            details,
        })),
        EnqueueOutcome::Busy => Ok(IntakeOutcome::Busy),
    }
}
