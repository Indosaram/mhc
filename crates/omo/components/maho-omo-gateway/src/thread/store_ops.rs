//! Every statement the gateway store runs.
//!
//! Upstream counterpart: `thread/gateway/store-ops.ts`.
//!
//! Wake protocol, sender half (`enqueue`): `BEGIN IMMEDIATE` takes the store's single write lock, the
//! row, receipt, causal edge and budgets are written, the target's inbox marker is created while the
//! lock is still held, then `COMMIT`. A receiver whose inbox watcher fires on the marker starts its
//! drain with its own `BEGIN IMMEDIATE` (`reconcile`), which cannot succeed until this transaction is
//! fully published or fully rolled back.

pub type AfterCommitCallback = Mutex<Option<Vec<Box<dyn FnOnce() + Send>>>>;
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rusqlite::types::Value as SqliteValue;
use rusqlite::Connection;
use serde_json::Value;

use super::constants::{
    GATEWAY_RECEIPT_RETENTION_MS, MAX_CAUSAL_DELIVERIES, MAX_FANOUT_PER_TURN, MAX_HOPS, PAIR_BUCKET_BURST, PAIR_BUCKET_REFILL_MS, QUEUED_TTL_MS, ROOT_LIFETIME_MS,
    TARGET_MAX_BYTES, TARGET_MAX_MESSAGES, is_durable_id,
};
use crate::paths::gateway_inbox_directory;
use super::provenance_record::is_provenance;
use super::schema::{GATEWAY_MIGRATIONS, GATEWAY_NEEDS_FK_OFF, GatewaySchemaVersionError};
use super::types::{
    ClaimOutcome, ClaimRequest, DeliveryRow, DeliveryState, DeliveryTransition, EffectiveLane, EnqueueOutcome, EnqueueRequest, GatewayStoreConfig,
    GatewayStoreEvent, GatewayStoreStats, OutcomeRecord, ProcessIdentity, ReconcileOutcome, ReconcileRequest, RecordOutcomeRequest, RefusalReason, StoredDeliveryEnvelope,
};

pub type SqlRow = BTreeMap<String, Value>;
pub type SqlParams = Vec<SqliteValue>;

/// The one failure a store operation gives up with when another process keeps the write lock past
/// `GATEWAY_LOCK_WAIT_MAX_MS`.
pub const LOCK_WAIT_EXCEEDED_CODE: &str = "gateway_lock_wait_exceeded";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{0}")]
    Sql(#[from] rusqlite::Error),
    #[error("{0}")]
    LockWait(String),
    #[error("{0}")]
    Schema(String),
    #[error("{0}")]
    Message(String),
}

impl StoreError {
    pub fn lock_wait_exceeded(op: &str, waited_ms: u64, limit_ms: u64) -> Self {
        Self::LockWait(format!("gateway store lock wait exceeded: {op} waited {waited_ms} ms for the write lock (limit {limit_ms} ms); another process holds it"))
    }

    pub fn code(&self) -> &'static str {
        match self {
            Self::LockWait(_) => LOCK_WAIT_EXCEEDED_CODE,
            Self::Schema(_) => "gateway_schema_too_new",
            _ => "internal_error",
        }
    }

    pub fn is_lock_wait_exceeded(&self) -> bool {
        matches!(self, Self::LockWait(_))
    }

    pub fn message(&self) -> String {
        self.to_string()
    }
}

impl From<GatewaySchemaVersionError> for StoreError {
    fn from(error: GatewaySchemaVersionError) -> Self {
        Self::Schema(error.to_string())
    }
}

pub fn v_str(value: impl Into<String>) -> SqliteValue {
    SqliteValue::Text(value.into())
}

pub fn v_opt_str(value: Option<impl Into<String>>) -> SqliteValue {
    value.map_or(SqliteValue::Null, |value| SqliteValue::Text(value.into()))
}

pub fn v_i64(value: i64) -> SqliteValue {
    SqliteValue::Integer(value)
}

pub fn v_opt_i64(value: Option<i64>) -> SqliteValue {
    value.map_or(SqliteValue::Null, SqliteValue::Integer)
}

fn json_value(value: &Value) -> Value {
    value.clone()
}

/// The statements a store operation runs, behind one seam so extension code can share them.
pub trait SqlAccess {
    fn exec(&self, sql: &str) -> Result<(), StoreError>;
    fn run(&self, sql: &str, params: &[SqliteValue]) -> Result<usize, StoreError>;
    fn one(&self, sql: &str, params: &[SqliteValue]) -> Result<Option<SqlRow>, StoreError>;
    fn all(&self, sql: &str, params: &[SqliteValue]) -> Result<Vec<SqlRow>, StoreError>;
}

fn row_to_map(row: &rusqlite::Row<'_>, columns: &[String]) -> Result<SqlRow, StoreError> {
    let mut mapped = SqlRow::new();
    for (index, name) in columns.iter().enumerate() {
        let value = match row.get_ref(index)? {
            rusqlite::types::ValueRef::Null => Value::Null,
            rusqlite::types::ValueRef::Integer(number) => Value::from(number),
            rusqlite::types::ValueRef::Real(number) => Value::from(number),
            rusqlite::types::ValueRef::Text(bytes) => Value::from(String::from_utf8_lossy(bytes).into_owned()),
            rusqlite::types::ValueRef::Blob(bytes) => Value::from(bytes.to_vec()),
        };
        mapped.insert(name.clone(), value);
    }
    Ok(mapped)
}

impl SqlAccess for StoreContext {
    fn exec(&self, sql: &str) -> Result<(), StoreError> {
        self.conn.execute_batch(sql)?;
        Ok(())
    }

    fn run(&self, sql: &str, params: &[SqliteValue]) -> Result<usize, StoreError> {
        let changed = self.conn.execute(sql, rusqlite::params_from_iter(params.iter()))?;
        self.stats.writes.fetch_add(changed as i64, Ordering::Relaxed);
        Ok(changed)
    }

    fn one(&self, sql: &str, params: &[SqliteValue]) -> Result<Option<SqlRow>, StoreError> {
        let mut statement = self.conn.prepare(sql)?;
        let columns: Vec<String> = statement.column_names().iter().map(|name| (*name).to_owned()).collect();
        let mut rows = statement.query(rusqlite::params_from_iter(params.iter()))?;
        match rows.next()? {
            Some(row) => Ok(Some(row_to_map(row, &columns)?)),
            None => Ok(None),
        }
    }

    fn all(&self, sql: &str, params: &[SqliteValue]) -> Result<Vec<SqlRow>, StoreError> {
        let mut statement = self.conn.prepare(sql)?;
        let columns: Vec<String> = statement.column_names().iter().map(|name| (*name).to_owned()).collect();
        let mut rows = statement.query(rusqlite::params_from_iter(params.iter()))?;
        let mut collected = Vec::new();
        while let Some(row) = rows.next()? {
            collected.push(row_to_map(row, &columns)?);
        }
        Ok(collected)
    }
}

pub fn row_str(row: &SqlRow, key: &str) -> String {
    row.get(key).map(|value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }).unwrap_or_default()
}

pub fn row_opt_str(row: &SqlRow, key: &str) -> Option<String> {
    match row.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(other) => Some(other.to_string()),
    }
}

pub fn row_i64(row: &SqlRow, key: &str) -> i64 {
    match row.get(key) {
        Some(Value::Number(number)) => number.as_i64().or_else(|| number.as_f64().map(|value| value as i64)).unwrap_or(0),
        Some(Value::String(text)) => text.parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn row_opt_i64(row: &SqlRow, key: &str) -> Option<i64> {
    match row.get(key) {
        None | Some(Value::Null) => None,
        _ => Some(row_i64(row, key)),
    }
}

/// A store context: one connection, its configuration and the transaction-local side effects.
pub struct StoreContext {
    pub conn: Connection,
    pub config: GatewayStoreConfig,
    pub self_identity: ProcessIdentity,
    pub stats: GatewayStoreStatsCounters,
    events: Mutex<Vec<GatewayStoreEvent>>,
    after_commit: AfterCommitCallback,
    after_rollback: AfterCommitCallback,
}

#[derive(Debug, Default)]
pub struct GatewayStoreStatsCounters {
    writes: AtomicI64,
    marker_unlinks: AtomicI64,
    transactions: AtomicI64,
}

impl GatewayStoreStatsCounters {
    pub fn snapshot(&self) -> GatewayStoreStats {
        GatewayStoreStats {
            writes: self.writes.load(Ordering::Relaxed),
            marker_unlinks: self.marker_unlinks.load(Ordering::Relaxed),
            transactions: self.transactions.load(Ordering::Relaxed),
        }
    }
}

impl StoreContext {
    pub fn new(conn: Connection, config: GatewayStoreConfig, self_identity: ProcessIdentity) -> Self {
        Self {
            conn,
            config,
            self_identity,
            stats: GatewayStoreStatsCounters::default(),
            events: Mutex::new(Vec::new()),
            after_commit: Mutex::new(None),
            after_rollback: Mutex::new(None),
        }
    }

    pub fn emit(&self, event: GatewayStoreEvent) {
        if let Ok(mut events) = self.events.lock() {
            events.push(event);
        }
    }

    pub fn drain_events(&self) -> Vec<GatewayStoreEvent> {
        self.events.lock().map(|mut events| std::mem::take(&mut *events)).unwrap_or_default()
    }

    pub fn joining_transaction(&self) -> bool {
        self.after_commit.lock().map(|state| state.is_some()).unwrap_or(false)
    }

    fn push_after_commit(&self, effect: Box<dyn FnOnce() + Send>) {
        if let Ok(mut state) = self.after_commit.lock()
            && let Some(effects) = state.as_mut()
        {
            effects.push(effect);
        }
    }

    fn push_after_rollback(&self, effect: Box<dyn FnOnce() + Send>) {
        if let Ok(mut state) = self.after_rollback.lock()
            && let Some(effects) = state.as_mut()
        {
            effects.push(effect);
        }
    }

    pub fn stats(&self) -> GatewayStoreStats {
        self.stats.snapshot()
    }
}

/// The agent directory the inbox markers live under.
fn agent_dir(ctx: &StoreContext) -> &Path {
    Path::new(&ctx.config.agent_dir)
}

fn is_busy(error: &rusqlite::Error) -> bool {
    matches!(error.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy) | Some(rusqlite::ErrorCode::DatabaseLocked))
}

/// `BEGIN IMMEDIATE`, retried once per busy timeout until the lock is taken or the total wait is spent.
pub fn begin_immediate(ctx: &StoreContext, op: &str, retry_until_locked: bool) -> Result<bool, StoreError> {
    if ctx.joining_transaction() {
        ctx.exec("SAVEPOINT gateway_enqueue")?;
        return Ok(true);
    }
    let started = Instant::now();
    loop {
        match ctx.conn.execute_batch("BEGIN IMMEDIATE") {
            Ok(()) => {
                ctx.stats.transactions.fetch_add(1, Ordering::Relaxed);
                return Ok(true);
            }
            Err(error) if is_busy(&error) => {
                ctx.emit(GatewayStoreEvent::Busy { op: op.to_owned() });
                if !retry_until_locked {
                    return Ok(false);
                }
                let waited = started.elapsed().as_millis() as u64;
                if waited + 2 * ctx.config.busy_timeout_ms > ctx.config.lock_wait_max_ms {
                    ctx.emit(GatewayStoreEvent::LockWaitExceeded { op: op.to_owned(), waited_ms: waited as i64 });
                    return Err(StoreError::lock_wait_exceeded(op, waited, ctx.config.lock_wait_max_ms));
                }
                std::thread::sleep(Duration::from_millis(ctx.config.busy_timeout_ms));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

pub fn rollback_quietly(ctx: &StoreContext) {
    let _ = ctx.conn.execute_batch(if ctx.joining_transaction() { "ROLLBACK TO gateway_enqueue; RELEASE gateway_enqueue" } else { "ROLLBACK" });
}

/// Runs `body` inside the store's write transaction (or the joined outer one).
pub fn transaction<T>(ctx: &StoreContext, op: &str, body: impl FnOnce() -> Result<T, StoreError>) -> Result<T, StoreError> {
    if ctx.joining_transaction() {
        return body();
    }
    begin_immediate(ctx, op, true)?;
    match body() {
        Ok(value) => {
            ctx.conn.execute_batch("COMMIT")?;
            Ok(value)
        }
        Err(error) => {
            rollback_quietly(ctx);
            Err(error)
        }
    }
}

fn create_marker(ctx: &StoreContext, target_durable_id: &str, delivery_id: &str, allow_existing: bool) -> Result<std::path::PathBuf, StoreError> {
    let directory = gateway_inbox_directory(agent_dir(ctx), target_durable_id);
    std::fs::create_dir_all(&directory).map_err(|error| StoreError::Message(error.to_string()))?;
    let path = directory.join(delivery_id);
    match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(mut file) => {
            use std::io::Write;
            let payload = serde_json::json!({ "pid": ctx.self_identity.pid, "process_start_time": ctx.self_identity.process_start_time });
            let _ = file.write_all(payload.to_string().as_bytes());
        }
        Err(error) if allow_existing && error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(path),
        Err(error) => return Err(StoreError::Message(error.to_string())),
    }
    let rollback_path = path.clone();
    ctx.push_after_rollback(Box::new(move || {
        let _ = std::fs::remove_file(&rollback_path);
    }));
    Ok(path)
}

pub fn unlink_marker(ctx: &StoreContext, target_durable_id: &str, delivery_id: &str) {
    if ctx.joining_transaction() {
        let path = gateway_inbox_directory(agent_dir(ctx), target_durable_id).join(delivery_id);
        ctx.push_after_commit(Box::new(move || {
            let _ = std::fs::remove_file(&path);
        }));
        return;
    }
    let path = gateway_inbox_directory(agent_dir(ctx), target_durable_id).join(delivery_id);
    match std::fs::remove_file(&path) {
        Ok(()) => {
            ctx.stats.marker_unlinks.fetch_add(1, Ordering::Relaxed);
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => {}
    }
}

fn marker_names(ctx: &StoreContext, target_durable_id: &str) -> Vec<String> {
    let directory = gateway_inbox_directory(agent_dir(ctx), target_durable_id);
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
        .filter(|name| !name.starts_with('.'))
        .collect()
}

const DELIVERY_COLUMNS: &[&str] = &[
    "delivery_id",
    "target_durable_id",
    "seq",
    "sender",
    "sender_turn",
    "envelope",
    "body",
    "bytes",
    "mode_requested",
    "mode_effective",
    "expected_turn_id",
    "state",
    "reason",
    "admitted_by",
    "claimed_at",
    "attempt",
    "admission_kind",
    "turn_epoch",
    "root_id",
    "hop",
    "created_at",
    "updated_at",
    "expires_at",
    "binding_id",
    "binding_revision",
    "actor_user_id",
];

pub const OPEN_STATES: &str = "('queued', 'admitting', 'admitted')";

fn row_from_record(record: &SqlRow) -> DeliveryRow {
    let envelope: StoredDeliveryEnvelope = serde_json::from_str(&row_str(record, "envelope")).unwrap_or(StoredDeliveryEnvelope {
        provenance: None,
        origin: super::types::EnvelopeOrigin::Session { session: String::new() },
        actor: String::new(),
        via: Vec::new(),
        root_id: String::new(),
        hop: 0,
        delivery_id: String::new(),
    });
    DeliveryRow {
        delivery_id: row_str(record, "delivery_id"),
        target_durable_id: row_str(record, "target_durable_id"),
        seq: row_i64(record, "seq"),
        sender: row_str(record, "sender"),
        sender_turn: row_opt_str(record, "sender_turn"),
        envelope,
        body: row_str(record, "body"),
        bytes: row_i64(record, "bytes"),
        mode_requested: super::types::GatewayDeliveryMode::parse(&row_str(record, "mode_requested")).unwrap_or(super::types::GatewayDeliveryMode::Auto),
        mode_effective: row_opt_str(record, "mode_effective").and_then(|lane| match lane.as_str() {
            "steer" => Some(EffectiveLane::Steer),
            "follow_up" => Some(EffectiveLane::FollowUp),
            _ => None,
        }),
        expected_turn_id: row_opt_i64(record, "expected_turn_id"),
        state: DeliveryState::parse(&row_str(record, "state")).unwrap_or(DeliveryState::Queued),
        reason: row_opt_str(record, "reason"),
        admitted_by: row_opt_str(record, "admitted_by").and_then(|json| serde_json::from_str(&json).ok()),
        claimed_at: row_opt_i64(record, "claimed_at"),
        attempt: row_i64(record, "attempt"),
        admission_kind: row_opt_str(record, "admission_kind"),
        turn_epoch: row_opt_i64(record, "turn_epoch"),
        root_id: row_str(record, "root_id"),
        hop: row_i64(record, "hop"),
        created_at: row_i64(record, "created_at"),
        updated_at: row_i64(record, "updated_at"),
        expires_at: row_i64(record, "expires_at"),
        binding_id: row_opt_str(record, "binding_id"),
        binding_revision: row_opt_i64(record, "binding_revision"),
        actor_user_id: row_opt_str(record, "actor_user_id"),
    }
}

fn select_rows(ctx: &StoreContext, where_clause: &str, params: &[SqliteValue], order_by: Option<&str>) -> Result<Vec<DeliveryRow>, StoreError> {
    let sql = format!("SELECT {} FROM deliveries WHERE {where_clause}{}", DELIVERY_COLUMNS.join(", "), order_by.map(|order| format!(" ORDER BY {order}")).unwrap_or_default());
    Ok(ctx.all(&sql, params)?.iter().map(row_from_record).collect())
}

pub fn select_row(ctx: &StoreContext, delivery_id: &str) -> Result<Option<DeliveryRow>, StoreError> {
    Ok(select_rows(ctx, "delivery_id = ?", &[v_str(delivery_id)], None)?.into_iter().next())
}

pub fn queue_position(ctx: &StoreContext, row: &DeliveryRow) -> Result<i64, StoreError> {
    if !row.state.is_open() {
        return Ok(0);
    }
    let found = ctx.one(
        &format!("SELECT COUNT(*) AS n FROM deliveries WHERE target_durable_id = ? AND state IN {OPEN_STATES} AND seq <= ?"),
        &[v_str(&row.target_durable_id), v_i64(row.seq)],
    )?;
    Ok(found.as_ref().map(|row| row_i64(row, "n")).unwrap_or(1))
}

fn allocate_seq(ctx: &StoreContext, target_durable_id: &str) -> Result<i64, StoreError> {
    let meta = ctx.one("SELECT next_seq FROM session_meta WHERE durable_id = ?", &[v_str(target_durable_id)])?;
    let seq = match meta {
        Some(meta) => row_i64(&meta, "next_seq"),
        None => ctx
            .one("SELECT COALESCE(MAX(seq), 0) + 1 AS m FROM deliveries WHERE target_durable_id = ?", &[v_str(target_durable_id)])?
            .as_ref()
            .map(|row| row_i64(row, "m"))
            .unwrap_or(1),
    };
    ctx.run(
        "INSERT INTO session_meta (durable_id, next_seq) VALUES (?, ?) ON CONFLICT(durable_id) DO UPDATE SET next_seq = excluded.next_seq",
        &[v_str(target_durable_id), v_i64(seq + 1)],
    )?;
    Ok(seq)
}

fn insert_delivery(ctx: &StoreContext, row: &DeliveryRow) -> Result<(), StoreError> {
    let placeholders = DELIVERY_COLUMNS.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let sql = format!("INSERT INTO deliveries ({}) VALUES ({placeholders})", DELIVERY_COLUMNS.join(", "));
    let envelope = serde_json::to_string(&row.envelope).unwrap_or_else(|_| "{}".to_owned());
    let admitted_by = row.admitted_by.as_ref().and_then(|identity| serde_json::to_string(identity).ok());
    ctx.run(
        &sql,
        &[
            v_str(&row.delivery_id),
            v_str(&row.target_durable_id),
            v_i64(row.seq),
            v_str(&row.sender),
            v_opt_str(row.sender_turn.clone()),
            v_str(envelope),
            v_str(&row.body),
            v_i64(row.bytes),
            v_str(row.mode_requested.as_str()),
            v_opt_str(row.mode_effective.map(EffectiveLane::as_str)),
            v_opt_i64(row.expected_turn_id),
            v_str(row.state.as_str()),
            v_opt_str(row.reason.clone()),
            v_opt_str(admitted_by),
            v_opt_i64(row.claimed_at),
            v_i64(row.attempt),
            v_opt_str(row.admission_kind.clone()),
            v_opt_i64(row.turn_epoch),
            v_str(&row.root_id),
            v_i64(row.hop),
            v_i64(row.created_at),
            v_i64(row.updated_at),
            v_i64(row.expires_at),
            v_opt_str(row.binding_id.clone()),
            v_opt_i64(row.binding_revision),
            v_opt_str(row.actor_user_id.clone()),
        ],
    )?;
    Ok(())
}

fn args_hash(value: &Value) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(serde_json::to_vec(value).unwrap_or_default());
    format!("{:x}", hasher.finalize())
}

/// A unique id for a row the gateway creates: the process, the clock and a counter, hashed.
pub fn new_id(prefix: &str) -> String {
    use sha2::Digest;
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|elapsed| elapsed.as_nanos()).unwrap_or(0);
    let mut hasher = sha2::Sha256::new();
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(nanos.to_le_bytes());
    hasher.update(counter.to_le_bytes());
    let digest = format!("{:x}", hasher.finalize());
    format!("{prefix}-{}", &digest[..32])
}

pub fn hash_args(value: &Value) -> String {
    args_hash(value)
}

pub fn schema_version(ctx: &StoreContext) -> Result<i64, StoreError> {
    let version = ctx
        .one("SELECT user_version FROM pragma_user_version()", &[])?
        .as_ref()
        .map(|row| row_i64(row, "user_version"))
        .unwrap_or(0);
    if version > GATEWAY_MIGRATIONS.len() as i64 {
        return Err(GatewaySchemaVersionError::new(version, GATEWAY_MIGRATIONS.len() as i64, "Gateway").into());
    }
    Ok(version)
}

/// Applies pending migrations; a current store takes no write lock.
pub fn migrate(ctx: &StoreContext) -> Result<(), StoreError> {
    loop {
        let version = schema_version(ctx)?;
        if version >= GATEWAY_MIGRATIONS.len() as i64 {
            return Ok(());
        }
        let needs_foreign_keys_off = GATEWAY_NEEDS_FK_OFF.contains(&(version as usize));
        if needs_foreign_keys_off {
            ctx.exec("PRAGMA foreign_keys = OFF")?;
        }
        let applied = transaction(ctx, "migrate", || {
            let current = schema_version(ctx)?;
            if current != version {
                return Ok(false);
            }
            for statement in GATEWAY_MIGRATIONS[current as usize] {
                ctx.exec(statement)?;
            }
            if needs_foreign_keys_off {
                let violations = ctx.all("SELECT parent FROM pragma_foreign_key_check", &[])?;
                if violations.iter().any(|row| row_str(row, "parent") == "bindings") {
                    return Err(StoreError::Message(format!(
                        "gateway schema {} would leave a foreign-key violation referencing bindings; staying at v{current}",
                        current + 1
                    )));
                }
            }
            ctx.exec(&format!("PRAGMA user_version = {}", current + 1))?;
            Ok(true)
        });
        if needs_foreign_keys_off {
            ctx.exec("PRAGMA foreign_keys = ON")?;
        }
        if !applied? {
            return Ok(());
        }
    }
}

fn refused(code: &str, message: impl Into<String>, details: Option<Value>) -> EnqueueOutcome {
    EnqueueOutcome::Refused {
        code: super::errors::ThreadErrorCode::parse(code).unwrap_or_else(|| panic!("unknown thread error code: {code}")),
        message: message.into(),
        details,
    }
}

struct ReceiptRecord {
    args_hash: String,
    status: String,
    delivery_id: Option<String>,
    owner_instance: String,
    result: Option<String>,
    expires_at: i64,
}

fn select_receipt(ctx: &StoreContext, principal: &str, key: &str) -> Result<Option<ReceiptRecord>, StoreError> {
    let found = ctx.one(
        "SELECT args_hash, status, delivery_id, owner_instance, result, expires_at FROM receipts WHERE principal = ? AND operation = 'deliver' AND idempotency_key = ?",
        &[v_str(principal), v_str(key)],
    )?;
    Ok(found.map(|row| ReceiptRecord {
        args_hash: row_str(&row, "args_hash"),
        status: row_str(&row, "status"),
        delivery_id: row_opt_str(&row, "delivery_id"),
        owner_instance: row_str(&row, "owner_instance"),
        result: row_opt_str(&row, "result"),
        expires_at: row_i64(&row, "expires_at"),
    }))
}

/// How a retry under an existing delivery receipt is answered.
fn classify_receipt(ctx: &StoreContext, request: &EnqueueRequest, receipt: &ReceiptRecord) -> Result<EnqueueOutcome, StoreError> {
    if receipt.args_hash != request.receipt.args_hash {
        return Ok(refused("idempotency_conflict", "The idempotency key was already used with different arguments.", None));
    }
    if receipt.status == "completed"
        && let Some(result) = &receipt.result
    {
        return Ok(EnqueueOutcome::Replay { result: serde_json::from_str(result).unwrap_or(Value::Null) });
    }
    let row = match &receipt.delivery_id {
        Some(delivery_id) => select_row(ctx, delivery_id)?,
        None => None,
    };
    let decided = row.as_ref().is_some_and(|row| matches!(row.state, DeliveryState::Admitted | DeliveryState::Applied | DeliveryState::Refused));
    if receipt.status == "prepared" && !decided && receipt.owner_instance == ctx.config.instance_id {
        return Ok(refused("idempotency_in_progress", "The same delivery is already in progress.", None));
    }
    if receipt.status == "prepared"
        && decided
        && let Some(row) = row
    {
        let result = super::result::result_from_row(&row, queue_position(ctx, &row)?, request.endpoint_kind, true, false);
        let result_value = serde_json::to_value(&result).unwrap_or(Value::Null);
        ctx.run(
            "UPDATE receipts SET status = 'completed', result = ?, updated_at = ? WHERE principal = ? AND operation = 'deliver' AND idempotency_key = ?",
            &[v_str(result_value.to_string()), v_i64(request.now), v_str(&request.sender_principal), v_str(&request.receipt.idempotency_key)],
        )?;
        return Ok(EnqueueOutcome::Replay { result: result_value });
    }
    if receipt.status == "prepared" {
        ctx.run(
            "UPDATE receipts SET status = 'uncertain', error_note = ?, updated_at = ? WHERE principal = ? AND operation = 'deliver' AND idempotency_key = ?",
            &[
                v_str("the sender lost the acknowledgement before the target admitted the delivery"),
                v_i64(request.now),
                v_str(&request.sender_principal),
                v_str(&request.receipt.idempotency_key),
            ],
        )?;
    }
    Ok(refused(
        "idempotency_uncertain",
        "The earlier delivery may or may not reach the target; it is never sent twice.",
        Some(serde_json::json!({ "delivery_id": receipt.delivery_id, "state": row.map(|row| row.state.as_str()) })),
    ))
}

struct CausalPlacement {
    root_id: String,
    hop: i64,
    via: Vec<String>,
    new_root: bool,
}

fn place_causally(ctx: &StoreContext, request: &EnqueueRequest) -> Result<Result<CausalPlacement, EnqueueOutcome>, StoreError> {
    let placement = match &request.cause_delivery_id {
        Some(cause_id) => {
            let cause = select_row(ctx, cause_id)?;
            let valid = cause.as_ref().is_some_and(|cause| {
                cause.target_durable_id == request.sender_node && matches!(cause.state, DeliveryState::Admitting | DeliveryState::Admitted | DeliveryState::Applied)
            });
            let Some(cause) = cause.filter(|_| valid) else {
                return Ok(Err(refused(
                    "invalid_arguments",
                    "The delivery this send continues is not one the sender received.",
                    Some(serde_json::json!({ "guard": "unknown_cause" })),
                )));
            };
            let mut via = cause.envelope.via.clone();
            via.push(request.sender_node.clone());
            CausalPlacement { root_id: cause.root_id.clone(), hop: cause.hop + 1, via, new_root: false }
        }
        None => CausalPlacement { root_id: new_id("root"), hop: 1, via: vec![request.sender_node.clone()], new_root: true },
    };
    if let Some(claimed) = &request.claimed_root_id
        && *claimed != placement.root_id
    {
        return Ok(Err(refused("invalid_arguments", "The causal root is derived by the gateway and cannot be chosen by the caller.", Some(serde_json::json!({ "guard": "forged_root" })))));
    }
    if !placement.new_root {
        let root = ctx.one("SELECT expires_at FROM causal_roots WHERE root_id = ?", &[v_str(&placement.root_id)])?;
        if root.as_ref().is_none_or(|root| row_i64(root, "expires_at") <= request.now) {
            return Ok(Err(refused("loop_detected", "This causal chain is older than its lifetime.", Some(serde_json::json!({ "guard": "root_expired", "root_id": placement.root_id })))));
        }
    }
    if request.target_durable_id == request.sender_node {
        return Ok(Err(refused("loop_detected", "A session cannot deliver to itself through the gateway.", Some(serde_json::json!({ "guard": "self_send" })))));
    }
    if placement.hop > MAX_HOPS {
        return Ok(Err(refused(
            "loop_detected",
            format!("This delivery would be hop {}; at most {MAX_HOPS} are allowed.", placement.hop),
            Some(serde_json::json!({ "guard": "hop_limit", "hop": placement.hop })),
        )));
    }
    let count = ctx
        .one("SELECT COUNT(*) AS n FROM deliveries WHERE root_id = ? AND state != 'refused'", &[v_str(&placement.root_id)])?
        .as_ref()
        .map(|row| row_i64(row, "n"))
        .unwrap_or(0);
    if count >= MAX_CAUSAL_DELIVERIES {
        return Ok(Err(refused(
            "loop_detected",
            format!("This causal chain already carried {count} deliveries."),
            Some(serde_json::json!({ "guard": "causal_budget", "root_id": placement.root_id })),
        )));
    }
    let edges: Vec<(String, String)> = ctx
        .all("SELECT from_durable_id, to_durable_id FROM causal_edges WHERE root_id = ?", &[v_str(&placement.root_id)])?
        .iter()
        .map(|row| (row_str(row, "from_durable_id"), row_str(row, "to_durable_id")))
        .collect();
    if closes_cycle(&edges, &request.sender_node, &request.target_durable_id) {
        return Ok(Err(refused("loop_detected", "The target already leads back to the sender in this causal chain.", Some(serde_json::json!({ "guard": "cycle", "root_id": placement.root_id })))));
    }
    Ok(Ok(placement))
}

/// A send closes a cycle when the target already reaches the sender inside one causal chain.
pub fn closes_cycle(edges: &[(String, String)], from: &str, to: &str) -> bool {
    let mut seen: Vec<&str> = Vec::new();
    let mut stack: Vec<&str> = vec![to];
    while let Some(node) = stack.pop() {
        if node == from {
            return true;
        }
        if seen.contains(&node) {
            continue;
        }
        seen.push(node);
        for (edge_from, edge_to) in edges {
            if edge_from == node {
                stack.push(edge_to);
            }
        }
    }
    false
}

fn check_fanout(ctx: &StoreContext, request: &EnqueueRequest) -> Result<Option<EnqueueOutcome>, StoreError> {
    let Some(sender_turn) = &request.sender_turn else {
        return Ok(None);
    };
    let targets: Vec<String> = ctx
        .all(
            "SELECT DISTINCT target_durable_id AS t FROM deliveries WHERE sender = ? AND sender_turn = ? AND state != 'refused'",
            &[v_str(&request.sender_principal), v_str(sender_turn)],
        )?
        .iter()
        .map(|row| row_str(row, "t"))
        .collect();
    if targets.contains(&request.target_durable_id) || (targets.len() as i64) < MAX_FANOUT_PER_TURN {
        return Ok(None);
    }
    Ok(Some(refused("overloaded", format!("One turn may reach at most {MAX_FANOUT_PER_TURN} sessions."), Some(serde_json::json!({ "budget": "fanout" })))))
}

fn take_pair_token(ctx: &StoreContext, request: &EnqueueRequest) -> Result<Option<EnqueueOutcome>, StoreError> {
    let sender = request.rate_principal.clone().unwrap_or_else(|| request.sender_principal.clone());
    let bucket = ctx.one("SELECT tokens, updated_at FROM rate_buckets WHERE sender = ? AND target_durable_id = ?", &[v_str(&sender), v_str(&request.target_durable_id)])?;
    let tokens = match &bucket {
        None => PAIR_BUCKET_BURST,
        Some(bucket) => {
            let stored = bucket.get("tokens").and_then(Value::as_f64).unwrap_or(0.0);
            let updated = row_i64(bucket, "updated_at");
            PAIR_BUCKET_BURST.min(stored + ((request.now - updated).max(0) as f64) / PAIR_BUCKET_REFILL_MS)
        }
    };
    if tokens < 1.0 {
        return Ok(Some(refused(
            "overloaded",
            "Too many deliveries to this session; wait before sending again.",
            Some(serde_json::json!({ "budget": "pair_rate", "retry_after_ms": ((1.0 - tokens) * PAIR_BUCKET_REFILL_MS).ceil() as i64 })),
        )));
    }
    ctx.run(
        "INSERT INTO rate_buckets (sender, target_durable_id, tokens, updated_at) VALUES (?, ?, ?, ?) ON CONFLICT(sender, target_durable_id) DO UPDATE SET tokens = excluded.tokens, updated_at = excluded.updated_at",
        &[v_str(&sender), v_str(&request.target_durable_id), SqliteValue::Real(tokens - 1.0), v_i64(request.now)],
    )?;
    Ok(None)
}

fn check_binding(ctx: &StoreContext, request: &EnqueueRequest) -> Result<Result<Option<i64>, EnqueueOutcome>, StoreError> {
    let Some(binding) = &request.binding else {
        return Ok(Ok(None));
    };
    let found = ctx.one(
        "SELECT revision, status, session_durable_id, direction_inbound, expires_at FROM bindings WHERE binding_id = ?",
        &[v_str(&binding.binding_id)],
    )?;
    let expires_at = found.as_ref().and_then(|row| row_opt_str(row, "expires_at")).and_then(|text| parse_rfc3339_millis(&text));
    let valid = found.as_ref().is_some_and(|row| {
        row_str(row, "status") == "active"
            && row_i64(row, "revision") == binding.revision
            && row_str(row, "session_durable_id") == request.target_durable_id
            && row_i64(row, "direction_inbound") == 1
            && expires_at.is_none_or(|expires| expires > request.now)
    });
    if !valid {
        return Ok(Err(refused(
            "invalid_arguments",
            "The binding is not active for this session at this revision.",
            Some(serde_json::json!({ "binding": binding.binding_id })),
        )));
    }
    Ok(Ok(expires_at))
}

/// A minimal RFC 3339 reader for the binding columns: `YYYY-MM-DDTHH:MM:SS[.fff]Z`.
pub fn parse_rfc3339_millis(text: &str) -> Option<i64> {
    let (date, rest) = text.split_once('T')?;
    let mut date_parts = date.split('-');
    let year: i64 = date_parts.next()?.parse().ok()?;
    let month: i64 = date_parts.next()?.parse().ok()?;
    let day: i64 = date_parts.next()?.parse().ok()?;
    let rest = rest.trim_end_matches('Z');
    let (time, fraction) = rest.split_once('.').unwrap_or((rest, ""));
    let mut time_parts = time.split(':');
    let hour: i64 = time_parts.next()?.parse().ok()?;
    let minute: i64 = time_parts.next()?.parse().ok()?;
    let second: i64 = time_parts.next()?.parse().ok()?;
    let millis: i64 = if fraction.is_empty() { 0 } else { format!("{fraction:0<3}")[..3].parse().ok()? };
    Some(civil_to_millis(year, month, day, hour, minute, second, millis))
}

fn civil_to_millis(year: i64, month: i64, day: i64, hour: i64, minute: i64, second: i64, millis: i64) -> i64 {
    // Days from 1970-01-01 to the given civil date (Howard Hinnant's algorithm).
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    ((days * 24 + hour) * 60 + minute) * 60_000 + second * 1_000 + millis
}

/// Formats `now` as an RFC 3339 UTC timestamp with milliseconds.
pub fn rfc3339(millis: i64) -> String {
    let total_seconds = millis.div_euclid(1_000);
    let ms = millis.rem_euclid(1_000);
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{ms:03}Z")
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The sender half of a delivery.
pub fn enqueue(ctx: &StoreContext, request: &EnqueueRequest) -> Result<EnqueueOutcome, StoreError> {
    let provenance = serde_json::to_value(&request.provenance).unwrap_or(Value::Null);
    if !is_provenance(&provenance) {
        return Ok(refused("invalid_arguments", "Delivery provenance is required.", None));
    }
    if !is_durable_id(&request.target_durable_id) || !is_durable_id(&request.delivery_id) {
        return Ok(refused("invalid_arguments", "The target session id is not a durable session id.", None));
    }
    if !begin_immediate(ctx, "enqueue", false)? {
        return Ok(EnqueueOutcome::Busy);
    }
    let mut marker: Option<std::path::PathBuf> = None;
    let mut committed = false;
    let commit_sql = if ctx.joining_transaction() { "RELEASE gateway_enqueue" } else { "COMMIT" };
    let outcome = (|| -> Result<EnqueueOutcome, StoreError> {
        let _ = delete_expired_receipt(ctx, &request.sender_principal, &request.receipt.idempotency_key, request.now);
        if let Some(receipt) = select_receipt(ctx, &request.sender_principal, &request.receipt.idempotency_key)? {
            let outcome = classify_receipt(ctx, request, &receipt)?;
            ctx.exec(commit_sql)?;
            committed = true;
            return Ok(outcome);
        }
        let binding_expiry = match check_binding(ctx, request)? {
            Ok(expiry) => expiry,
            Err(refusal) => {
                rollback_quietly(ctx);
                return Ok(refusal);
            }
        };
        let backlog = ctx.one(
            &format!("SELECT COUNT(*) AS n, COALESCE(SUM(bytes), 0) AS b FROM deliveries WHERE target_durable_id = ? AND state IN {OPEN_STATES}"),
            &[v_str(&request.target_durable_id)],
        )?;
        let bytes = request.body.len() as i64;
        let pending_messages = backlog.as_ref().map(|row| row_i64(row, "n")).unwrap_or(0);
        let pending_bytes = backlog.as_ref().map(|row| row_i64(row, "b")).unwrap_or(0);
        if pending_messages >= TARGET_MAX_MESSAGES || pending_bytes + bytes > TARGET_MAX_BYTES as i64 {
            rollback_quietly(ctx);
            return Ok(refused(
                "queue_full",
                "The target's delivery backlog is full.",
                Some(serde_json::json!({ "max_messages": TARGET_MAX_MESSAGES, "max_bytes": TARGET_MAX_BYTES })),
            ));
        }
        let placement = match place_causally(ctx, request)? {
            Ok(placement) => placement,
            Err(refusal) => {
                rollback_quietly(ctx);
                return Ok(refusal);
            }
        };
        if let Some(budget) = check_fanout(ctx, request)?.or(take_pair_token(ctx, request)?) {
            rollback_quietly(ctx);
            return Ok(budget);
        }
        if placement.new_root {
            ctx.run(
                "INSERT INTO causal_roots (root_id, origin_principal, created_at, expires_at) VALUES (?, ?, ?, ?)",
                &[v_str(&placement.root_id), v_str(&request.sender_principal), v_i64(request.now), v_i64(request.now + ROOT_LIFETIME_MS)],
            )?;
        }
        ctx.run(
            "INSERT OR IGNORE INTO causal_edges (root_id, from_durable_id, to_durable_id, delivery_id, created_at) VALUES (?, ?, ?, ?, ?)",
            &[v_str(&placement.root_id), v_str(&request.sender_node), v_str(&request.target_durable_id), v_str(&request.delivery_id), v_i64(request.now)],
        )?;
        let row = DeliveryRow {
            delivery_id: request.delivery_id.clone(),
            target_durable_id: request.target_durable_id.clone(),
            seq: allocate_seq(ctx, &request.target_durable_id)?,
            sender: request.sender_principal.clone(),
            sender_turn: request.sender_turn.clone(),
            envelope: StoredDeliveryEnvelope {
                provenance: Some(request.provenance.clone()),
                origin: request.origin.clone(),
                actor: request.actor.clone(),
                via: placement.via.clone(),
                root_id: placement.root_id.clone(),
                hop: placement.hop,
                delivery_id: request.delivery_id.clone(),
            },
            body: request.body.clone(),
            bytes,
            mode_requested: request.mode,
            mode_effective: None,
            expected_turn_id: request.expected_turn_id,
            state: DeliveryState::Queued,
            reason: None,
            admitted_by: None,
            claimed_at: None,
            attempt: 0,
            admission_kind: None,
            turn_epoch: None,
            root_id: placement.root_id.clone(),
            hop: placement.hop,
            created_at: request.now,
            updated_at: request.now,
            expires_at: QUEUED_TTL_MS.saturating_add(request.now).min(binding_expiry.unwrap_or(i64::MAX)),
            binding_id: request.binding.as_ref().map(|binding| binding.binding_id.clone()),
            binding_revision: request.binding.as_ref().map(|binding| binding.revision),
            actor_user_id: request.origin.external().and_then(|origin| origin.author.as_ref()).and_then(|author| author.user_id.clone()),
        };
        insert_delivery(ctx, &row)?;
        ctx.run(
            "INSERT INTO receipts (principal, operation, idempotency_key, args_hash, status, delivery_id, owner_instance, created_at, updated_at, expires_at) VALUES (?, 'deliver', ?, ?, 'prepared', ?, ?, ?, ?, ?)",
            &[
                v_str(&request.sender_principal),
                v_str(&request.receipt.idempotency_key),
                v_str(&request.receipt.args_hash),
                v_str(&row.delivery_id),
                v_str(&ctx.config.instance_id),
                v_i64(request.now),
                v_i64(request.now),
                v_i64(request.now + GATEWAY_RECEIPT_RETENTION_MS),
            ],
        )?;
        marker = Some(create_marker(ctx, &row.target_durable_id, &row.delivery_id, false)?);
        let position = queue_position(ctx, &row)?;
        ctx.exec(commit_sql)?;
        committed = true;
        Ok(EnqueueOutcome::Inserted { row: Box::new(row), queue_position: position })
    })();
    if outcome.is_err() && !committed {
        rollback_quietly(ctx);
        if let Some(marker) = marker {
            let _ = std::fs::remove_file(marker);
        }
    }
    outcome
}

fn delete_expired_receipt(ctx: &StoreContext, principal: &str, key: &str, now: i64) -> Result<usize, StoreError> {
    ctx.run(
        "DELETE FROM receipts WHERE principal = ? AND operation = 'deliver' AND idempotency_key = ? AND expires_at <= ?",
        &[v_str(principal), v_str(key), v_i64(now)],
    )
}

fn claim_fingerprint(row: &DeliveryRow) -> String {
    let admitted = row.admitted_by.as_ref().and_then(|identity| serde_json::to_string(identity).ok()).unwrap_or_else(|| "null".to_owned());
    format!("{admitted}#{}", row.attempt)
}

const CLEARED_CLAIM: &str = "admitted_by = NULL, claimed_at = NULL, mode_effective = NULL, admission_kind = NULL, turn_epoch = NULL";

fn transition(ctx: &StoreContext, row: &DeliveryRow, to: DeliveryState, now: i64, reason: Option<RefusalReason>) -> Result<(), StoreError> {
    if to == DeliveryState::Queued {
        ctx.run(&format!("UPDATE deliveries SET state = 'queued', {CLEARED_CLAIM}, updated_at = ? WHERE delivery_id = ?"), &[v_i64(now), v_str(&row.delivery_id)])?;
        return Ok(());
    }
    if to == DeliveryState::Refused {
        ctx.run("UPDATE deliveries SET state = 'refused', reason = ?, updated_at = ? WHERE delivery_id = ?", &[v_opt_str(reason.map(RefusalReason::as_str)), v_i64(now), v_str(&row.delivery_id)])?;
        return Ok(());
    }
    ctx.run("UPDATE deliveries SET state = ?, updated_at = ? WHERE delivery_id = ?", &[v_str(to.as_str()), v_i64(now), v_str(&row.delivery_id)])?;
    if to == DeliveryState::Applied {
        ctx.run("UPDATE session_meta SET applied_seq = MAX(applied_seq, ?) WHERE durable_id = ?", &[v_i64(row.seq), v_str(&row.target_durable_id)])?;
    }
    Ok(())
}

fn same_process(left: &ProcessIdentity, right: &ProcessIdentity) -> bool {
    left.instance_id == right.instance_id && left.pid == right.pid && left.process_start_time == right.process_start_time
}

/// Receiver barrier pass: claim verdicts are read before the lock, then applied under `BEGIN IMMEDIATE`
/// only to rows whose claim is still the one that was judged.
pub fn reconcile(ctx: &StoreContext, request: &ReconcileRequest) -> Result<ReconcileOutcome, StoreError> {
    let target = &request.target_durable_id;
    let rows = select_rows(ctx, "target_durable_id = ? AND state IN ('admitting', 'admitted')", &[v_str(target)], None)?;
    let mut verdicts: BTreeMap<String, (String, DeliveryState)> = BTreeMap::new();
    for row in &rows {
        let Some(claimant) = &row.admitted_by else {
            continue;
        };
        if same_process(claimant, &request.self_identity) {
            continue;
        }
        // A claimant of another process is judged by its own transcript ledger, which this store
        // cannot read: an unproven claim is left alone (dual runtime), never re-queued blindly.
        let dead = claimant_is_dead(claimant);
        let to = if dead { DeliveryState::Queued } else { DeliveryState::Uncertain };
        verdicts.insert(row.delivery_id.clone(), (claim_fingerprint(row), to));
    }
    if ctx.config.test_hooks.announce_barrier {
        ctx.emit(GatewayStoreEvent::Barrier { op: "reconcile".to_owned() });
    }
    transaction(ctx, "reconcile", || {
        let pending: Vec<&String> = request.ledger.pending.iter().collect();
        let emitted: Vec<&String> = request.ledger.emitted.iter().collect();
        let mut transitions = Vec::new();
        let mut dual = Vec::new();
        for row in select_rows(ctx, &format!("target_durable_id = ? AND state IN {OPEN_STATES}"), &[v_str(target)], Some("seq"))? {
            let to;
            if row.state == DeliveryState::Queued {
                if row.expires_at <= request.now {
                    transition(ctx, &row, DeliveryState::Refused, request.now, Some(RefusalReason::Expired))?;
                    transitions.push(DeliveryTransition { delivery_id: row.delivery_id.clone(), from: row.state, to: DeliveryState::Refused });
                }
                continue;
            }
            match &row.admitted_by {
                Some(claimant) if same_process(claimant, &request.self_identity) => {
                    to = if emitted.contains(&&row.delivery_id) {
                        DeliveryState::Applied
                    } else if pending.contains(&&row.delivery_id) {
                        DeliveryState::Admitted
                    } else {
                        DeliveryState::Queued
                    };
                }
                None => to = DeliveryState::Queued,
                Some(_) => match verdicts.get(&row.delivery_id) {
                    Some((fingerprint, verdict)) if *fingerprint == claim_fingerprint(&row) && *verdict != DeliveryState::Uncertain => to = *verdict,
                    _ => {
                        dual.push(row.delivery_id.clone());
                        continue;
                    }
                },
            }
            if to == row.state {
                continue;
            }
            transition(ctx, &row, to, request.now, None)?;
            transitions.push(DeliveryTransition { delivery_id: row.delivery_id.clone(), from: row.state, to });
        }
        for name in marker_names(ctx, target) {
            let state = ctx.one("SELECT state FROM deliveries WHERE delivery_id = ?", &[v_str(&name)])?.map(|row| row_str(&row, "state"));
            if state.as_deref() != Some("queued") {
                unlink_marker(ctx, target, &name);
            }
        }
        let queued = select_rows(ctx, "target_durable_id = ? AND state = 'queued'", &[v_str(target)], Some("seq"))?;
        Ok(ReconcileOutcome { queued, transitions, dual_runtime: dual })
    })
}

/// Whether another process's claim can be let go: its pid is gone, or its start time moved.
fn claimant_is_dead(claimant: &ProcessIdentity) -> bool {
    if claimant.pid <= 0 {
        return false;
    }
    let alive = Path::new(&format!("/proc/{}", claimant.pid)).exists();
    !alive
}

pub fn claim(ctx: &StoreContext, request: &ClaimRequest) -> Result<ClaimOutcome, StoreError> {
    transaction(ctx, "claim", || {
        let Some(row) = select_row(ctx, &request.delivery_id)? else {
            return Ok(ClaimOutcome::Lost { state: None });
        };
        if row.state != DeliveryState::Queued {
            return Ok(ClaimOutcome::Lost { state: Some(row.state) });
        }
        if row.expires_at <= request.now {
            transition(ctx, &row, DeliveryState::Refused, request.now, Some(RefusalReason::Expired))?;
            unlink_marker(ctx, &row.target_durable_id, &row.delivery_id);
            return Ok(ClaimOutcome::Lost { state: Some(DeliveryState::Refused) });
        }
        let claimed_by = serde_json::to_string(&request.self_identity).unwrap_or_default();
        ctx.run(
            "UPDATE deliveries SET state = 'admitting', admitted_by = ?, claimed_at = ?, attempt = attempt + 1, mode_effective = ?, updated_at = ? WHERE delivery_id = ? AND state = 'queued'",
            &[v_str(claimed_by), v_i64(request.now), v_str(request.lane.as_str()), v_i64(request.now), v_str(&request.delivery_id)],
        )?;
        unlink_marker(ctx, &row.target_durable_id, &row.delivery_id);
        match select_row(ctx, &request.delivery_id)? {
            Some(row) => Ok(ClaimOutcome::Claimed { row: Box::new(row) }),
            None => Ok(ClaimOutcome::Lost { state: None }),
        }
    })
}

pub fn record_outcome(ctx: &StoreContext, request: &RecordOutcomeRequest) -> Result<ClaimOutcome, StoreError> {
    transaction(ctx, "record_outcome", || {
        let Some(row) = select_row(ctx, &request.delivery_id)? else {
            return Ok(ClaimOutcome::Lost { state: None });
        };
        let owned = matches!(row.state, DeliveryState::Admitting | DeliveryState::Admitted) && row.admitted_by.as_ref().is_some_and(|claimant| same_process(claimant, &request.self_identity));
        if !owned {
            return Ok(ClaimOutcome::Lost { state: Some(row.state) });
        }
        match &request.outcome {
            OutcomeRecord::Requeue => transition(ctx, &row, DeliveryState::Queued, request.now, None)?,
            OutcomeRecord::Refused { reason } => transition(ctx, &row, DeliveryState::Refused, request.now, Some(*reason))?,
            OutcomeRecord::Admitted { admission_kind, turn_epoch } | OutcomeRecord::Applied { admission_kind, turn_epoch } => {
                ctx.run(
                    "UPDATE deliveries SET admission_kind = ?, turn_epoch = ?, updated_at = ? WHERE delivery_id = ?",
                    &[v_str(admission_kind.clone()), v_i64(*turn_epoch), v_i64(request.now), v_str(&row.delivery_id)],
                )?;
                let to = if matches!(&request.outcome, OutcomeRecord::Applied { .. }) { DeliveryState::Applied } else { DeliveryState::Admitted };
                if row.state != to {
                    transition(ctx, &row, to, request.now, None)?;
                }
            }
        }
        match select_row(ctx, &request.delivery_id)? {
            Some(row) => Ok(ClaimOutcome::Claimed { row: Box::new(row) }),
            None => Ok(ClaimOutcome::Lost { state: None }),
        }
    })
}

pub fn refuse_queued(ctx: &StoreContext, now: i64, delivery_id: &str, reason: RefusalReason) -> Result<bool, StoreError> {
    transaction(ctx, "refuse", || {
        let Some(row) = select_row(ctx, delivery_id)? else {
            return Ok(false);
        };
        if row.state != DeliveryState::Queued {
            return Ok(false);
        }
        transition(ctx, &row, DeliveryState::Refused, now, Some(reason))?;
        unlink_marker(ctx, &row.target_durable_id, &row.delivery_id);
        Ok(true)
    })
}

pub fn complete_receipt(ctx: &StoreContext, now: i64, principal: &str, idempotency_key: &str, result: &Value) -> Result<bool, StoreError> {
    transaction(ctx, "complete_receipt", || {
        let changed = ctx.run(
            "UPDATE receipts SET status = 'completed', result = ?, updated_at = ? WHERE principal = ? AND operation = 'deliver' AND idempotency_key = ? AND status = 'prepared' AND owner_instance = ?",
            &[v_str(result.to_string()), v_i64(now), v_str(principal), v_str(idempotency_key), v_str(&ctx.config.instance_id)],
        )?;
        Ok(changed == 1)
    })
}

pub fn abandon_receipt(ctx: &StoreContext, now: i64, principal: &str, idempotency_key: &str, error_note: &str) -> Result<bool, StoreError> {
    transaction(ctx, "abandon_receipt", || {
        let changed = ctx.run(
            "UPDATE receipts SET status = 'uncertain', error_note = ?, updated_at = ? WHERE principal = ? AND operation = 'deliver' AND idempotency_key = ? AND status = 'prepared' AND owner_instance = ?",
            &[v_str(error_note), v_i64(now), v_str(principal), v_str(idempotency_key), v_str(&ctx.config.instance_id)],
        )?;
        Ok(changed == 1)
    })
}

/// The stored result of a completed receipt, and the row facts its arguments hash was taken over.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryReceipt {
    pub args_hash: String,
    pub completed: bool,
    pub result: Value,
    pub target_durable_id: String,
    pub binding_revision: Option<i64>,
}

pub fn delivery_receipt(ctx: &StoreContext, now: i64, principal: &str, idempotency_key: &str) -> Result<Option<DeliveryReceipt>, StoreError> {
    let Some(receipt) = select_receipt(ctx, principal, idempotency_key)? else {
        return Ok(None);
    };
    if receipt.expires_at <= now {
        return Ok(None);
    }
    let Some(delivery_id) = receipt.delivery_id.clone() else {
        return Ok(None);
    };
    let Some(row) = select_row(ctx, &delivery_id)? else {
        return Ok(None);
    };
    let completed = receipt.status == "completed" && receipt.result.is_some();
    Ok(Some(DeliveryReceipt {
        args_hash: receipt.args_hash,
        completed,
        result: if completed { serde_json::from_str(receipt.result.as_deref().unwrap_or("null")).unwrap_or(Value::Null) } else { Value::Null },
        target_durable_id: row.target_durable_id,
        binding_revision: row.binding_revision,
    }))
}

pub fn recover_delivery(ctx: &StoreContext, now: i64, principal: &str, idempotency_key: &str, args_hash: &str) -> Result<Option<EnqueueOutcome>, StoreError> {
    transaction(ctx, "recover_delivery", || {
        let Some(receipt) = select_receipt(ctx, principal, idempotency_key)? else {
            return Ok(None);
        };
        if receipt.expires_at <= now {
            return Ok(None);
        }
        let request = EnqueueRequest {
            provenance: super::types::Provenance { workspace: "unclassified".into(), project: "recover".into(), lane: None, session: None },
            now,
            delivery_id: receipt.delivery_id.clone().unwrap_or_default(),
            target_durable_id: String::new(),
            sender_principal: principal.to_owned(),
            rate_principal: None,
            sender_node: String::new(),
            sender_turn: None,
            cause_delivery_id: None,
            claimed_root_id: None,
            origin: super::types::EnvelopeOrigin::Session { session: String::new() },
            actor: String::new(),
            body: String::new(),
            mode: super::types::GatewayDeliveryMode::Auto,
            expected_turn_id: None,
            binding: None,
            receipt: super::types::ReceiptRef { idempotency_key: idempotency_key.to_owned(), args_hash: args_hash.to_owned() },
            endpoint_kind: None,
        };
        Ok(Some(classify_receipt(ctx, &request, &receipt)?))
    })
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct DeliveryView {
    pub row: DeliveryRow,
    pub queue_position: i64,
}

pub fn delivery_view(ctx: &StoreContext, delivery_id: &str) -> Result<Option<DeliveryView>, StoreError> {
    match select_row(ctx, delivery_id)? {
        Some(row) => {
            let position = queue_position(ctx, &row)?;
            Ok(Some(DeliveryView { row, queue_position: position }))
        }
        None => Ok(None),
    }
}

pub fn list_deliveries(ctx: &StoreContext, target_durable_id: Option<&str>, root_id: Option<&str>) -> Result<Vec<DeliveryRow>, StoreError> {
    if let Some(target) = target_durable_id {
        return select_rows(ctx, "target_durable_id = ?", &[v_str(target)], Some("seq"));
    }
    if let Some(root) = root_id {
        return select_rows(ctx, "root_id = ?", &[v_str(root)], Some("created_at, seq"));
    }
    select_rows(ctx, "1 = 1", &[], Some("created_at, seq"))
}

pub fn is_referenced(ctx: &StoreContext, durable_id: &str) -> Result<bool, StoreError> {
    let open = ctx
        .one(&format!("SELECT COUNT(*) AS n FROM deliveries WHERE target_durable_id = ? AND state IN {OPEN_STATES}"), &[v_str(durable_id)])?
        .as_ref()
        .map(|row| row_i64(row, "n"))
        .unwrap_or(0);
    if open > 0 {
        return Ok(true);
    }
    let bindings = ctx
        .one("SELECT COUNT(*) AS n FROM bindings WHERE session_durable_id = ? AND status = 'active'", &[v_str(durable_id)])?
        .as_ref()
        .map(|row| row_i64(row, "n"))
        .unwrap_or(0);
    Ok(bindings > 0 || !marker_names(ctx, durable_id).is_empty())
}

pub fn journal_mode(ctx: &StoreContext) -> Result<String, StoreError> {
    Ok(ctx
        .one("SELECT journal_mode FROM pragma_journal_mode()", &[])?
        .as_ref()
        .map(|row| row_str(row, "journal_mode"))
        .unwrap_or_default())
}

/// Opens a store connection with the pragmas every gateway store runs with.
pub fn open_connection(path: &Path, busy_timeout_ms: u64) -> Result<Connection, StoreError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| StoreError::Message(error.to_string()))?;
    }
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_millis(busy_timeout_ms))?;
    conn.execute_batch("PRAGMA journal_mode = WAL; PRAGMA foreign_keys = ON")?;
    Ok(conn)
}

#[cfg(test)]
impl StoreContext {
    pub fn test_config() -> GatewayStoreConfig {
        GatewayStoreConfig {
            agent_dir: std::env::temp_dir().join("maho-gateway-test").to_string_lossy().into_owned(),
            busy_timeout_ms: 50,
            lock_wait_max_ms: 500,
            instance_id: "test-instance".to_owned(),
            runtime_instance: None,
            legacy_mailbox_directories: Vec::new(),
            test_hooks: Default::default(),
        }
    }

    pub fn test_identity() -> ProcessIdentity {
        ProcessIdentity { pid: std::process::id() as i64, process_start_time: None, instance_id: "test-instance".to_owned(), runtime_instance: None }
    }
}

/// The claiming process's start time, so a later claim of the same pid is recognised as another process.
pub fn self_identity_now() -> Option<String> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let rest = stat.rsplit_once(')')?.1.trim();
    rest.split_whitespace().nth(19).map(str::to_owned)
}

/// A record a caller reads back without the delivery machinery.
pub fn json_string(value: &Value) -> String {
    json_value(value).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_round_trips() {
        let millis = 1_767_225_600_123;
        assert_eq!(parse_rfc3339_millis(&rfc3339(millis)), Some(millis));
        assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn a_cycle_is_closed_when_the_target_reaches_the_sender() {
        let edges = vec![("a".to_owned(), "b".to_owned()), ("b".to_owned(), "c".to_owned())];
        assert!(closes_cycle(&edges, "c", "a"));
        assert!(closes_cycle(&edges, "a", "a"));
        assert!(closes_cycle(&edges, "b", "a"));
        assert!(!closes_cycle(&edges, "a", "c"));
        assert!(!closes_cycle(&[], "a", "b"));
    }
}
