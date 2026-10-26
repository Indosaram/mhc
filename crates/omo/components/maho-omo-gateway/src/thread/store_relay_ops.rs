//! The relay side of the gateway store: bindings, the outbox, reply tokens and completion arms.
//!
//! Upstream counterpart: `thread/gateway/store-relay-ops.ts`. Every mutation runs in one store
//! transaction that first applies due expiries; the TTL is never extended.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::store_ops::{SqlAccess, StoreContext, StoreError, parse_rfc3339_millis, rfc3339, row_i64, row_opt_i64, row_opt_str, row_str, transaction, v_i64, v_opt_i64, v_opt_str, v_str};
use super::types::{ExternalAuthor, StoreRefusal};

pub const BINDING_SCHEMA_VERSION: i64 = 1;
pub const BINDING_PLATFORMS: [&str; 8] = ["discord", "telegram", "slack", "notion", "feishu", "herdr", "custom", "whatsapp"];
pub const OUTBOUND_EVENTS: [&str; 4] = ["milestone", "report", "question", "completion"];
pub const INBOUND_MODES: [&str; 2] = ["auto", "follow_up"];
/// The lane a binding's inbound message may ask for.
pub type InboundMode = String;
pub const COMPLETION_OUTCOMES: [&str; 3] = ["completed", "failed", "cancelled"];
/// One week; `ttl_seconds: null` (no expiry) only when the caller asks for it explicitly.
pub const BINDING_DEFAULT_TTL_SECONDS: i64 = 604_800;
pub const BINDING_MAX_TTL_SECONDS: i64 = 366 * 24 * 60 * 60;
/// A platform with no threads binds its whole chat under this thread id.
pub const WHOLE_CHAT_THREAD_ID: &str = "@chat";
pub const BINDING_DEFAULT_POLICY_ID: &str = "default";
pub const BINDINGS_PAGE_DEFAULT: i64 = 50;
pub const BINDINGS_PAGE_MAX: i64 = 200;
pub const OUTBOX_PAGE_DEFAULT: i64 = 100;
pub const OUTBOX_PAGE_MAX: i64 = 500;
/// The one relay text cap, in UTF-8 bytes.
pub const RELAY_TEXT_MAX_BYTES: usize = 32_768;
const IDENTIFIER_MAX_LENGTH: usize = 256;

/// A question row a caller closed another way: the session no longer waits on it.
pub const CLOSED_ELSEWHERE: &str = "The session closed this question another way, so there is nothing left to answer";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BindingStatus {
    Active,
    Detached,
    Expired,
}

impl BindingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Detached => "detached",
            Self::Expired => "expired",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "detached" => Some(Self::Detached),
            "expired" => Some(Self::Expired),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingDirection {
    pub inbound: bool,
    pub outbound: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingRecord {
    pub schema_version: i64,
    pub binding_id: String,
    pub revision: i64,
    pub status: BindingStatus,
    pub platform: String,
    pub account_id: String,
    pub chat_id: String,
    pub thread_id: String,
    pub root_message_id: Option<String>,
    pub progress_message_id: Option<String>,
    pub session_realm_id: String,
    pub session_durable_id: String,
    pub direction: BindingDirection,
    pub inbound_mode: String,
    pub outbound_events: Vec<String>,
    pub policy_id: String,
    pub created_at: String,
    pub updated_at: String,
    pub lease_started_at: String,
    pub ttl_seconds: Option<i64>,
    pub expires_at: Option<String>,
}

/// What a caller may leave out of a bind; `ttl_seconds: null` means no expiry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindInput {
    pub platform: String,
    pub account_id: String,
    pub chat_id: String,
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub root_message_id: Option<String>,
    #[serde(default)]
    pub progress_message_id: Option<String>,
    pub session_durable_id: String,
    #[serde(default)]
    pub direction: Option<BindingDirection>,
    #[serde(default)]
    pub inbound_mode: Option<String>,
    #[serde(default)]
    pub outbound_events: Option<Vec<String>>,
    #[serde(default)]
    pub policy_id: Option<String>,
    #[serde(default)]
    pub ttl_seconds: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxRow {
    pub cursor: i64,
    pub binding_id: String,
    pub revision: i64,
    pub event: String,
    pub text: String,
    pub state: String,
    pub created_at: String,
    pub edit_message_id: Option<String>,
    pub provider_message_id: Option<String>,
    pub reply_token: Option<String>,
    pub question_state: Option<String>,
    pub outcome: Option<String>,
    pub answered_by: Option<ExternalAuthor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutboxPage {
    pub binding_id: String,
    pub revision: i64,
    pub status: String,
    pub rows: Vec<OutboxRow>,
    pub next_cursor: i64,
    pub acked_cursor: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BindingsFilter {
    #[serde(default)]
    pub session_durable_id: Option<String>,
    #[serde(default)]
    pub platform: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    #[serde(default)]
    pub chat_id: Option<String>,
    #[serde(default)]
    pub status: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportOpRequest {
    pub session_durable_id: String,
    pub binding_id: Option<String>,
    #[serde(default)]
    pub origin_delivery_ids: Vec<String>,
    pub origin_local_input: bool,
    pub event: String,
    pub text: String,
    pub ui_request_id: Option<String>,
    pub ui_request_kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReportOpResult {
    pub binding_id: String,
    pub revision: i64,
    pub event: String,
    pub cursor: Option<i64>,
    pub reply_token: Option<String>,
    pub armed: bool,
}

/// A claimed answer: its row, the session it belongs to and the question it answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerClaim {
    pub binding_id: String,
    pub cursor: i64,
    pub session_durable_id: String,
    pub ui_request_id: String,
    pub ui_request_kind: Option<String>,
    pub claimed_at: i64,
    pub taken_over: Option<PriorAnswer>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorAnswer {
    pub answer: Option<String>,
    pub answered_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerClaimRef {
    pub reply_token: String,
    pub claimed_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerDelivered {
    pub reply_token: String,
    pub claimed_at: i64,
    pub answer: String,
    pub answered_by: Option<ExternalAuthor>,
}

/// `{ kind: "ok", ... }` or a store refusal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RelayOutcome<T> {
    Ok {
        #[serde(flatten)]
        value: T,
    },
    Refused {
        code: super::errors::ThreadErrorCode,
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        details: Option<Value>,
    },
}

impl<T> RelayOutcome<T> {
    pub fn ok(value: T) -> Self {
        Self::Ok { value }
    }

    pub fn from_refusal(refusal: StoreRefusal) -> Self {
        Self::Refused { code: refusal.code, message: refusal.message, details: refusal.details }
    }
}

fn refused(code: &str, message: impl Into<String>) -> StoreRefusal {
    StoreRefusal {
        code: super::errors::ThreadErrorCode::parse(code).unwrap_or_else(|| panic!("unknown thread error code: {code}")),
        message: message.into(),
        details: None,
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty() && value.chars().count() <= IDENTIFIER_MAX_LENGTH
}

/// Validates a bind input into the record a store writes.
pub fn normalize_bind_input(input: &BindInput) -> Result<BindInput, StoreRefusal> {
    if !BINDING_PLATFORMS.contains(&input.platform.as_str()) {
        return Err(refused("invalid_arguments", format!("platform {} is not a binding platform.", input.platform)));
    }
    for (label, value) in [("account_id", &input.account_id), ("chat_id", &input.chat_id), ("session_durable_id", &input.session_durable_id)] {
        if !identifier(value) {
            return Err(refused("invalid_arguments", format!("{label} must be a non-empty identifier of at most {IDENTIFIER_MAX_LENGTH} characters.")));
        }
    }
    let direction = input.direction.unwrap_or(BindingDirection { inbound: true, outbound: true });
    if !direction.inbound && !direction.outbound {
        return Err(refused("invalid_arguments", "A binding needs at least one direction."));
    }
    let inbound_mode = input.inbound_mode.clone().unwrap_or_else(|| "auto".to_owned());
    if !INBOUND_MODES.contains(&inbound_mode.as_str()) {
        return Err(refused("invalid_arguments", format!("inbound_mode must be one of {INBOUND_MODES:?}.")));
    }
    let outbound_events = input.outbound_events.clone().unwrap_or_else(|| OUTBOUND_EVENTS.iter().map(|event| (*event).to_owned()).collect());
    if let Some(unknown) = outbound_events.iter().find(|event| !OUTBOUND_EVENTS.contains(&event.as_str())) {
        return Err(refused("invalid_arguments", format!("{unknown} is not an outbound event.")));
    }
    let ttl_seconds = match input.ttl_seconds {
        None => Some(BINDING_DEFAULT_TTL_SECONDS),
        Some(ttl) if ttl <= 0 || ttl > BINDING_MAX_TTL_SECONDS => {
            return Err(refused("invalid_arguments", format!("ttl_seconds must be from 1 to {BINDING_MAX_TTL_SECONDS}.")));
        }
        Some(ttl) => Some(ttl),
    };
    Ok(BindInput {
        platform: input.platform.clone(),
        account_id: input.account_id.clone(),
        chat_id: input.chat_id.clone(),
        thread_id: Some(input.thread_id.clone().unwrap_or_else(|| WHOLE_CHAT_THREAD_ID.to_owned())),
        root_message_id: input.root_message_id.clone(),
        progress_message_id: input.progress_message_id.clone(),
        session_durable_id: input.session_durable_id.clone(),
        direction: Some(direction),
        inbound_mode: Some(inbound_mode),
        outbound_events: Some(outbound_events),
        policy_id: Some(input.policy_id.clone().unwrap_or_else(|| BINDING_DEFAULT_POLICY_ID.to_owned())),
        ttl_seconds,
    })
}

pub fn hash_args(value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(value).unwrap_or_default());
    format!("{:x}", hasher.finalize())
}

fn binding_from_row(row: &super::store_ops::SqlRow) -> BindingRecord {
    BindingRecord {
        schema_version: row_i64(row, "schema_version"),
        binding_id: row_str(row, "binding_id"),
        revision: row_i64(row, "revision"),
        status: BindingStatus::parse(&row_str(row, "status")).unwrap_or(BindingStatus::Detached),
        platform: row_str(row, "platform"),
        account_id: row_str(row, "account_id"),
        chat_id: row_str(row, "chat_id"),
        thread_id: row_str(row, "thread_id"),
        root_message_id: row_opt_str(row, "root_message_id"),
        progress_message_id: row_opt_str(row, "progress_message_id"),
        session_realm_id: row_str(row, "session_realm_id"),
        session_durable_id: row_str(row, "session_durable_id"),
        direction: BindingDirection { inbound: row_i64(row, "direction_inbound") == 1, outbound: row_i64(row, "direction_outbound") == 1 },
        inbound_mode: row_str(row, "inbound_mode"),
        outbound_events: serde_json::from_str(&row_str(row, "outbound_events")).unwrap_or_default(),
        policy_id: row_str(row, "policy_id"),
        created_at: row_str(row, "created_at"),
        updated_at: row_str(row, "updated_at"),
        lease_started_at: row_str(row, "lease_started_at"),
        ttl_seconds: row_opt_i64(row, "ttl_seconds"),
        expires_at: row_opt_str(row, "expires_at"),
    }
}

const BINDING_COLUMNS: &str = "seq, binding_id, schema_version, revision, status, platform, account_id, chat_id, thread_id, root_message_id, progress_message_id, session_realm_id, session_durable_id, direction_inbound, direction_outbound, inbound_mode, outbound_events, policy_id, created_at, updated_at, lease_started_at, ttl_seconds, expires_at";

/// The bindings a where clause selects, in binding-id order.
pub fn select_bindings(sql: &dyn SqlAccess, where_clause: &str, params: &[rusqlite::types::Value]) -> Result<Vec<BindingRecord>, StoreError> {
    let rows = sql.all(&format!("SELECT {BINDING_COLUMNS} FROM bindings WHERE {where_clause} ORDER BY binding_id"), params)?;
    Ok(rows.iter().map(binding_from_row).collect())
}

fn expire_due(ctx: &StoreContext, now: i64) -> Result<(), StoreError> {
    let stamp = rfc3339(now);
    ctx.run("UPDATE bindings SET status = 'expired', updated_at = ? WHERE status = 'active' AND expires_at IS NOT NULL AND expires_at <= ?", &[v_str(&stamp), v_str(&stamp)])?;
    Ok(())
}

fn realm_id(ctx: &StoreContext) -> String {
    ctx.one("SELECT value FROM gateway_meta WHERE key = 'realm_id'", &[]).ok().flatten().map(|row| row_str(&row, "value")).unwrap_or_default()
}

fn token_secret(ctx: &StoreContext) -> String {
    ctx.one("SELECT value FROM gateway_meta WHERE key = 'token_secret'", &[]).ok().flatten().map(|row| row_str(&row, "value")).unwrap_or_default()
}

/// A reply token binds one question row to its binding, revision and cursor under the store's secret.
pub fn reply_token(secret: &str, binding_id: &str, revision: i64, cursor: i64) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher.update(binding_id.as_bytes());
    hasher.update(revision.to_le_bytes());
    hasher.update(cursor.to_le_bytes());
    format!("rt_{cursor:x}{}", &format!("{:x}", hasher.finalize())[..32])
}

/// The bindings snapshot cursor: the rowid watermark and the last binding id on the page.
pub fn encode_cursor(seq: i64, binding_id: &str) -> String {
    let mut encoded = String::new();
    for byte in binding_id.as_bytes() {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("{seq}:{encoded}")
}

pub fn decode_cursor(cursor: &str) -> Option<(i64, String)> {
    let (seq, encoded) = cursor.split_once(':')?;
    let seq: i64 = seq.parse().ok()?;
    if encoded.len() % 2 != 0 {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..encoded.len() / 2).map(|index| u8::from_str_radix(&encoded[index * 2..index * 2 + 2], 16).ok()).collect();
    Some((seq, String::from_utf8(bytes?).ok()?))
}

pub fn bind_thread(ctx: &StoreContext, now: i64, input: &BindInput) -> Result<RelayOutcome<BindingRecord>, StoreError> {
    let input = match normalize_bind_input(input) {
        Ok(input) => input,
        Err(refusal) => return Ok(RelayOutcome::from_refusal(refusal)),
    };
    transaction(ctx, "bind", || {
        expire_due(ctx, now)?;
        let platform = input.platform.clone();
        let account_id = input.account_id.clone();
        let chat_id = input.chat_id.clone();
        let thread_id = input.thread_id.clone().unwrap_or_else(|| WHOLE_CHAT_THREAD_ID.to_owned());
        let held = select_bindings(ctx, "platform = ? AND account_id = ? AND chat_id = ? AND thread_id = ? AND status = 'active'", &[v_str(&platform), v_str(&account_id), v_str(&chat_id), v_str(&thread_id)])?;
        if let Some(held) = held.first() {
            if held.session_durable_id == input.session_durable_id {
                return Ok(RelayOutcome::ok(held.clone()));
            }
            return Ok(RelayOutcome::Refused {
                code: super::errors::ThreadErrorCode::parse("binding_conflict").unwrap_or_else(|| panic!("known code")),
                message: format!("This thread is already bound to session {}.", held.session_durable_id),
                details: Some(serde_json::json!({ "binding_id": held.binding_id, "session_durable_id": held.session_durable_id })),
            });
        }
        let stamp = rfc3339(now);
        let expires_at = input.ttl_seconds.map(|ttl| rfc3339(now + ttl * 1_000));
        let binding_id = super::store_ops::new_id("bind");
        let direction = input.direction.unwrap_or(BindingDirection { inbound: true, outbound: true });
        let events = serde_json::to_string(&input.outbound_events.clone().unwrap_or_default()).unwrap_or_else(|_| "[]".to_owned());
        ctx.run(
            "INSERT INTO bindings (binding_id, schema_version, revision, status, platform, account_id, chat_id, thread_id, root_message_id, progress_message_id, session_realm_id, session_durable_id, direction_inbound, direction_outbound, inbound_mode, outbound_events, policy_id, created_at, updated_at, lease_started_at, ttl_seconds, expires_at) VALUES (?, ?, ?, 'active', ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            &[
                v_str(&binding_id),
                v_i64(BINDING_SCHEMA_VERSION),
                v_i64(1),
                v_str(&platform),
                v_str(&account_id),
                v_str(&chat_id),
                v_str(&thread_id),
                v_opt_str(input.root_message_id.clone()),
                v_opt_str(input.progress_message_id.clone()),
                v_str(realm_id(ctx)),
                v_str(&input.session_durable_id),
                v_i64(i64::from(direction.inbound)),
                v_i64(i64::from(direction.outbound)),
                v_str(input.inbound_mode.clone().unwrap_or_else(|| "auto".to_owned())),
                v_str(events),
                v_str(input.policy_id.clone().unwrap_or_else(|| BINDING_DEFAULT_POLICY_ID.to_owned())),
                v_str(&stamp),
                v_str(&stamp),
                v_str(&stamp),
                v_opt_i64(input.ttl_seconds),
                v_opt_str(expires_at),
            ],
        )?;
        let created = select_bindings(ctx, "binding_id = ?", &[v_str(&binding_id)])?.into_iter().next();
        match created {
            Some(binding) => Ok(RelayOutcome::ok(binding)),
            None => Ok(RelayOutcome::from_refusal(refused("internal_error", "The binding was not written."))),
        }
    })
}

pub fn binding_view(ctx: &StoreContext, now: i64, binding_id: &str) -> Result<Option<BindingRecord>, StoreError> {
    transaction(ctx, "binding_view", || {
        expire_due(ctx, now)?;
        Ok(select_bindings(ctx, "binding_id = ?", &[v_str(binding_id)])?.into_iter().next())
    })
}

pub fn binding_for(ctx: &StoreContext, lookup: &super::store_extensions::ExtensionBindingLookup) -> Result<Option<BindingRecord>, StoreError> {
    Ok(select_bindings(
        ctx,
        "platform = ? AND account_id = ? AND chat_id = ? AND thread_id = ? AND status = 'active'",
        &[v_str(&lookup.platform), v_str(&lookup.account_id), v_str(&lookup.chat_id), v_str(&lookup.thread_id)],
    )?
    .into_iter()
    .next())
}

/// The session's active bindings, not expired at `now`, ordered by binding id.
pub fn bindings_for_session(ctx: &StoreContext, session_durable_id: &str, now: i64) -> Result<Vec<BindingRecord>, StoreError> {
    select_bindings(
        ctx,
        "session_durable_id = ? AND status = 'active' AND (expires_at IS NULL OR expires_at > ?)",
        &[v_str(session_durable_id), v_str(rfc3339(now))],
    )
}

pub fn list_bindings(ctx: &StoreContext, now: i64, filter: &BindingsFilter, cursor: Option<&str>, limit: Option<i64>) -> super::store_api::BindingPageResult {
    transaction(ctx, "list_bindings", || {
        expire_due(ctx, now)?;
        let mut clauses: Vec<String> = Vec::new();
        let mut params: Vec<rusqlite::types::Value> = Vec::new();
        if let Some(session) = &filter.session_durable_id {
            clauses.push("session_durable_id = ?".to_owned());
            params.push(v_str(session));
        }
        if let Some(platform) = &filter.platform {
            clauses.push("platform = ?".to_owned());
            params.push(v_str(platform));
        }
        if let Some(account) = &filter.account_id {
            clauses.push("account_id = ?".to_owned());
            params.push(v_str(account));
        }
        if let Some(chat) = &filter.chat_id {
            clauses.push("chat_id = ?".to_owned());
            params.push(v_str(chat));
        }
        if let Some(status) = &filter.status {
            clauses.push("status = ?".to_owned());
            params.push(v_str(status));
        }
        if let Some(cursor) = cursor {
            let Some((seq, binding_id)) = decode_cursor(cursor) else {
                return Ok(RelayOutcome::from_refusal(refused("cursor_invalid", "The bindings cursor is not one this gateway wrote.")));
            };
            clauses.push("(seq < ? OR (seq = ? AND binding_id > ?))".to_owned());
            params.push(v_i64(seq));
            params.push(v_i64(seq));
            params.push(v_str(binding_id));
        }
        let page = limit.unwrap_or(BINDINGS_PAGE_DEFAULT).clamp(1, BINDINGS_PAGE_MAX);
        let where_clause = if clauses.is_empty() { "1 = 1".to_owned() } else { clauses.join(" AND ") };
        let rows = ctx.all(&format!("SELECT {BINDING_COLUMNS} FROM bindings WHERE {where_clause} ORDER BY seq DESC, binding_id ASC LIMIT ?"), &{
            let mut with_limit = params.clone();
            with_limit.push(v_i64(page + 1));
            with_limit
        })?;
        let bindings: Vec<BindingRecord> = rows.iter().take(page as usize).map(binding_from_row).collect();
        let next_cursor = (rows.len() as i64 > page).then(|| {
            rows.get(page as usize - 1).map(|row| encode_cursor(row_i64(row, "seq"), &row_str(row, "binding_id"))).unwrap_or_default()
        });
        Ok(RelayOutcome::ok((bindings, next_cursor)))
    })
}

pub fn unbind_thread(ctx: &StoreContext, now: i64, binding_id: &str, expected_revision: i64) -> Result<RelayOutcome<(BindingRecord, bool, Vec<String>)>, StoreError> {
    transaction(ctx, "unbind", || {
        expire_due(ctx, now)?;
        let Some(binding) = select_bindings(ctx, "binding_id = ?", &[v_str(binding_id)])?.into_iter().next() else {
            return Ok(RelayOutcome::from_refusal(refused("not_found", "No binding has this id.")));
        };
        if binding.revision != expected_revision {
            return Ok(RelayOutcome::Refused {
                code: super::errors::ThreadErrorCode::parse("stale_revision").unwrap_or_else(|| panic!("known code")),
                message: format!("The binding is at revision {}.", binding.revision),
                details: Some(serde_json::json!({ "revision": binding.revision })),
            });
        }
        let in_flight: Vec<String> = ctx
            .all("SELECT delivery_id FROM deliveries WHERE binding_id = ? AND state IN ('queued', 'admitting', 'admitted')", &[v_str(binding_id)])?
            .iter()
            .map(|row| row_str(row, "delivery_id"))
            .collect();
        let already_closed = binding.status != BindingStatus::Active;
        if !already_closed {
            let stamp = rfc3339(now);
            ctx.run("UPDATE bindings SET status = 'detached', revision = revision + 1, updated_at = ? WHERE binding_id = ?", &[v_str(&stamp), v_str(binding_id)])?;
            for delivery_id in &in_flight {
                ctx.run("UPDATE deliveries SET state = 'refused', reason = 'binding_closed', updated_at = ? WHERE delivery_id = ? AND state = 'queued'", &[v_i64(now), v_str(delivery_id)])?;
            }
        }
        let updated = select_bindings(ctx, "binding_id = ?", &[v_str(binding_id)])?.into_iter().next().unwrap_or(binding);
        Ok(RelayOutcome::ok((updated, already_closed, in_flight)))
    })
}

pub fn rebind_thread(ctx: &StoreContext, now: i64, binding_id: &str, expected_revision: i64, session_durable_id: &str) -> Result<RelayOutcome<(BindingRecord, Vec<String>)>, StoreError> {
    transaction(ctx, "rebind", || {
        expire_due(ctx, now)?;
        let Some(binding) = select_bindings(ctx, "binding_id = ?", &[v_str(binding_id)])?.into_iter().next() else {
            return Ok(RelayOutcome::from_refusal(refused("not_found", "No binding has this id.")));
        };
        if binding.revision != expected_revision {
            return Ok(RelayOutcome::Refused {
                code: super::errors::ThreadErrorCode::parse("stale_revision").unwrap_or_else(|| panic!("known code")),
                message: format!("The binding is at revision {}.", binding.revision),
                details: Some(serde_json::json!({ "revision": binding.revision })),
            });
        }
        let closed: Vec<String> = ctx
            .all("SELECT delivery_id FROM deliveries WHERE binding_id = ? AND state = 'queued'", &[v_str(binding_id)])?
            .iter()
            .map(|row| row_str(row, "delivery_id"))
            .collect();
        let stamp = rfc3339(now);
        ctx.run(
            "UPDATE bindings SET status = 'active', revision = revision + 1, session_durable_id = ?, lease_started_at = ?, updated_at = ? WHERE binding_id = ?",
            &[v_str(session_durable_id), v_str(&stamp), v_str(&stamp), v_str(binding_id)],
        )?;
        for delivery_id in &closed {
            ctx.run("UPDATE deliveries SET state = 'refused', reason = 'binding_closed', updated_at = ? WHERE delivery_id = ? AND state = 'queued'", &[v_i64(now), v_str(delivery_id)])?;
        }
        let updated = select_bindings(ctx, "binding_id = ?", &[v_str(binding_id)])?.into_iter().next().unwrap_or(binding);
        Ok(RelayOutcome::ok((updated, closed)))
    })
}

fn outbox_row(row: &super::store_ops::SqlRow) -> OutboxRow {
    OutboxRow {
        cursor: row_i64(row, "cursor"),
        binding_id: row_str(row, "binding_id"),
        revision: row_i64(row, "revision"),
        event: row_str(row, "event_kind"),
        text: serde_json::from_str::<Value>(&row_str(row, "payload")).ok().and_then(|payload| payload.get("text").and_then(Value::as_str).map(str::to_owned)).unwrap_or_default(),
        state: row_str(row, "state"),
        created_at: rfc3339(row_i64(row, "created_at")),
        edit_message_id: row_opt_str(row, "provider_message_id"),
        provider_message_id: row_opt_str(row, "provider_message_id"),
        reply_token: row_opt_str(row, "reply_token"),
        question_state: row_opt_str(row, "question_state"),
        outcome: row_opt_str(row, "outcome"),
        answered_by: row_opt_str(row, "answered_by").and_then(|json| serde_json::from_str(&json).ok()),
    }
}

fn acked_cursor(ctx: &StoreContext, binding_id: &str) -> Result<i64, StoreError> {
    Ok(ctx
        .one("SELECT acked_cursor FROM outbox_cursors WHERE binding_id = ?", &[v_str(binding_id)])?
        .as_ref()
        .map(|row| row_i64(row, "acked_cursor"))
        .unwrap_or(0))
}

/// The session settled: armed completions become outbox rows with this outcome.
pub fn emit_completions(ctx: &StoreContext, now: i64, session_durable_id: &str, outcome: &str, through_arm_seq: Option<i64>) -> Result<Vec<(String, i64)>, StoreError> {
    transaction(ctx, "emit_completions", || {
        let arms = ctx.all(
            "SELECT arm_seq, binding_id, revision, text FROM completion_arms WHERE session_durable_id = ? ORDER BY arm_seq",
            &[v_str(session_durable_id)],
        )?;
        let mut emitted = Vec::new();
        for arm in arms {
            let arm_seq = row_i64(&arm, "arm_seq");
            if let Some(through) = through_arm_seq
                && arm_seq > through
            {
                continue;
            }
            let binding_id = row_str(&arm, "binding_id");
            let payload = serde_json::json!({ "text": row_str(&arm, "text"), "outcome": outcome });
            ctx.run(
                "INSERT INTO outbox (binding_id, revision, event_kind, payload, state, created_at, session_durable_id, outcome) VALUES (?, ?, 'completion', ?, 'pending', ?, ?, ?)",
                &[v_str(&binding_id), v_i64(row_i64(&arm, "revision")), v_str(payload.to_string()), v_i64(now), v_str(session_durable_id), v_str(outcome)],
            )?;
            let cursor = ctx
                .one("SELECT MAX(cursor) AS cursor FROM outbox WHERE binding_id = ?", &[v_str(&binding_id)])?
                .as_ref()
                .map(|row| row_i64(row, "cursor"))
                .unwrap_or(0);
            ctx.run("DELETE FROM completion_arms WHERE arm_seq = ?", &[v_i64(arm_seq)])?;
            emitted.push((binding_id, cursor));
        }
        Ok(emitted)
    })
}

pub fn pending_completion_arms(ctx: &StoreContext, durable_id: &str) -> Result<i64, StoreError> {
    Ok(ctx
        .one("SELECT COUNT(*) AS n FROM completion_arms WHERE session_durable_id = ?", &[v_str(durable_id)])?
        .as_ref()
        .map(|row| row_i64(row, "n"))
        .unwrap_or(0))
}

pub fn latest_completion_arm(ctx: &StoreContext, durable_id: &str) -> Result<Option<i64>, StoreError> {
    Ok(ctx
        .one("SELECT MAX(arm_seq) AS arm FROM completion_arms WHERE session_durable_id = ?", &[v_str(durable_id)])?
        .and_then(|row| row_opt_i64(&row, "arm")))
}

/// `thread_report`: writes one outbox row (or arms a completion) under the session's active binding.
pub fn report_event(ctx: &StoreContext, now: i64, request: &ReportOpRequest) -> Result<RelayOutcome<ReportOpResult>, StoreError> {
    transaction(ctx, "report", || {
        expire_due(ctx, now)?;
        let binding = match &request.binding_id {
            Some(binding_id) => select_bindings(ctx, "binding_id = ?", &[v_str(binding_id)])?.into_iter().next(),
            None => select_bindings(ctx, "session_durable_id = ? AND status = 'active'", &[v_str(&request.session_durable_id)])?.into_iter().next(),
        };
        let Some(binding) = binding else {
            return Ok(RelayOutcome::from_refusal(refused("not_found", "This session has no active binding.")));
        };
        if binding.session_durable_id != request.session_durable_id {
            return Ok(RelayOutcome::from_refusal(refused("scope_denied", "The binding is attached to another session.")));
        }
        if binding.status != BindingStatus::Active {
            return Ok(RelayOutcome::from_refusal(refused("binding_inactive", format!("The binding is {}.", binding.status.as_str()))));
        }
        if !binding.direction.outbound {
            return Ok(RelayOutcome::from_refusal(refused("unsupported", "The binding carries no outbound direction.")));
        }
        if !binding.outbound_events.contains(&request.event) {
            return Ok(RelayOutcome::from_refusal(refused("unsupported", format!("The binding does not subscribe to {}.", request.event))));
        }
        if request.text.len() > RELAY_TEXT_MAX_BYTES {
            return Ok(RelayOutcome::from_refusal(refused("message_too_large", format!("The text is {} bytes, above the {RELAY_TEXT_MAX_BYTES}-byte limit.", request.text.len()))));
        }
        if request.event == "completion" {
            ctx.run(
                "INSERT INTO completion_arms (session_durable_id, binding_id, revision, text, armed_at) VALUES (?, ?, ?, ?, ?)",
                &[v_str(&request.session_durable_id), v_str(&binding.binding_id), v_i64(binding.revision), v_str(&request.text), v_i64(now)],
            )?;
            return Ok(RelayOutcome::ok(ReportOpResult {
                binding_id: binding.binding_id,
                revision: binding.revision,
                event: request.event.clone(),
                cursor: None,
                reply_token: None,
                armed: true,
            }));
        }
        let payload = serde_json::json!({
            "text": request.text,
            "origin_delivery_ids": request.origin_delivery_ids,
            "origin_local_input": request.origin_local_input,
            "ui_request_id": request.ui_request_id,
            "ui_request_kind": request.ui_request_kind,
        });
        ctx.run(
            "INSERT INTO outbox (binding_id, revision, event_kind, payload, state, created_at, session_durable_id, question_state, ui_request_id, ui_request_kind) VALUES (?, ?, ?, ?, 'pending', ?, ?, ?, ?, ?)",
            &[
                v_str(&binding.binding_id),
                v_i64(binding.revision),
                v_str(&request.event),
                v_str(payload.to_string()),
                v_i64(now),
                v_str(&request.session_durable_id),
                v_opt_str((request.event == "question").then(|| "pending".to_owned())),
                v_opt_str(request.ui_request_id.clone()),
                v_opt_str(request.ui_request_kind.clone()),
            ],
        )?;
        let cursor = ctx
            .one("SELECT MAX(cursor) AS cursor FROM outbox WHERE binding_id = ?", &[v_str(&binding.binding_id)])?
            .as_ref()
            .map(|row| row_i64(row, "cursor"))
            .unwrap_or(0);
        let token = (request.event == "question").then(|| {
            let token = reply_token(&token_secret(ctx), &binding.binding_id, binding.revision, cursor);
            let _ = ctx.run("UPDATE outbox SET reply_token = ? WHERE cursor = ?", &[v_str(&token), v_i64(cursor)]);
            token
        });
        Ok(RelayOutcome::ok(ReportOpResult {
            binding_id: binding.binding_id,
            revision: binding.revision,
            event: request.event.clone(),
            cursor: Some(cursor),
            reply_token: token,
            armed: false,
        }))
    })
}

pub fn read_outbox(ctx: &StoreContext, now: i64, binding_id: &str, after_cursor: Option<i64>, limit: Option<i64>, pending_only: bool) -> Result<RelayOutcome<OutboxPage>, StoreError> {
    transaction(ctx, "read_outbox", || {
        expire_due(ctx, now)?;
        let Some(binding) = select_bindings(ctx, "binding_id = ?", &[v_str(binding_id)])?.into_iter().next() else {
            return Ok(RelayOutcome::from_refusal(refused("not_found", "No binding has this id.")));
        };
        let page = limit.unwrap_or(OUTBOX_PAGE_DEFAULT).clamp(1, OUTBOX_PAGE_MAX);
        let after = after_cursor.unwrap_or(0);
        let state_clause = if pending_only { " AND state = 'pending'" } else { "" };
        let rows = ctx.all(
            &format!("SELECT cursor, binding_id, revision, event_kind, payload, state, provider_message_id, created_at, reply_token, question_state, outcome, answered_by FROM outbox WHERE binding_id = ? AND cursor > ?{state_clause} ORDER BY cursor LIMIT ?"),
            &[v_str(binding_id), v_i64(after), v_i64(page)],
        )?;
        let rows: Vec<OutboxRow> = rows.iter().map(outbox_row).collect();
        let next_cursor = rows.last().map(|row| row.cursor).unwrap_or(after);
        Ok(RelayOutcome::ok(OutboxPage {
            binding_id: binding.binding_id,
            revision: binding.revision,
            status: binding.status.as_str().to_owned(),
            rows,
            next_cursor,
            acked_cursor: acked_cursor(ctx, binding_id)?,
        }))
    })
}

pub fn ack_outbox(ctx: &StoreContext, now: i64, binding_id: &str, cursor: i64, provider_message_id: Option<&str>) -> Result<RelayOutcome<(i64, bool)>, StoreError> {
    transaction(ctx, "ack_outbox", || {
        let Some(_binding) = select_bindings(ctx, "binding_id = ?", &[v_str(binding_id)])?.into_iter().next() else {
            return Ok(RelayOutcome::from_refusal(refused("not_found", "No binding has this id.")));
        };
        let known = ctx.one("SELECT cursor FROM outbox WHERE binding_id = ? AND cursor = ?", &[v_str(binding_id), v_i64(cursor)])?;
        if known.is_none() {
            return Ok(RelayOutcome::from_refusal(refused("cursor_invalid", "No outbox row has this cursor.")));
        }
        let current = acked_cursor(ctx, binding_id)?;
        if cursor <= current {
            return Ok(RelayOutcome::ok((current, false)));
        }
        ctx.run("UPDATE outbox SET state = 'acked', acked_at = ?, provider_message_id = COALESCE(?, provider_message_id) WHERE binding_id = ? AND cursor <= ?", &[v_i64(now), v_opt_str(provider_message_id.map(str::to_owned)), v_str(binding_id), v_i64(cursor)])?;
        ctx.run(
            "INSERT INTO outbox_cursors (binding_id, acked_cursor, updated_at) VALUES (?, ?, ?) ON CONFLICT(binding_id) DO UPDATE SET acked_cursor = excluded.acked_cursor, updated_at = excluded.updated_at",
            &[v_str(binding_id), v_i64(cursor), v_i64(now)],
        )?;
        Ok(RelayOutcome::ok((cursor, true)))
    })
}

pub fn claim_answer(ctx: &StoreContext, now: i64, binding_id: &str, token: &str, answer: &str, answered_by: Option<&ExternalAuthor>) -> Result<RelayOutcome<AnswerClaim>, StoreError> {
    transaction(ctx, "claim_answer", || {
        let Some(binding) = select_bindings(ctx, "binding_id = ?", &[v_str(binding_id)])?.into_iter().next() else {
            return Ok(RelayOutcome::from_refusal(refused("not_found", "No binding has this id.")));
        };
        let Some(row) = ctx.one("SELECT cursor, revision, question_state, answer_state, answer, answered_at, ui_request_id, ui_request_kind, session_durable_id FROM outbox WHERE reply_token = ?", &[v_str(token)])? else {
            return Ok(RelayOutcome::from_refusal(refused("stale_token", "No question carries this reply token.")));
        };
        if row_str(&row, "session_durable_id") != binding.session_durable_id {
            return Ok(RelayOutcome::from_refusal(refused("binding_mismatch", "The question was asked through another binding.")));
        }
        match row_opt_str(&row, "question_state").as_deref() {
            Some("answered") => {
                if row_opt_str(&row, "answer_state").as_deref() == Some("delivered") {
                    return Ok(RelayOutcome::from_refusal(refused("already_answered", "The question has its answer.")));
                }
                return Ok(RelayOutcome::from_refusal(refused("answer_in_progress", "Another answer is still being handed to the session.")));
            }
            Some("expired") | Some("cancelled") => {
                return Ok(RelayOutcome::from_refusal(refused("already_answered", CLOSED_ELSEWHERE)));
            }
            _ => {}
        }
        let prior = row_opt_str(&row, "answer").map(|answer| PriorAnswer { answer: Some(answer), answered_at: row_i64(&row, "answered_at") });
        let answered_by_json = answered_by.and_then(|author| serde_json::to_string(author).ok());
        let cursor = row_i64(&row, "cursor");
        ctx.run(
            "UPDATE outbox SET question_state = 'answered', answer = ?, answered_at = ?, answered_by = ?, answer_state = 'in_flight' WHERE cursor = ?",
            &[v_str(answer), v_i64(now), v_opt_str(answered_by_json), v_i64(cursor)],
        )?;
        Ok(RelayOutcome::ok(AnswerClaim {
            binding_id: binding.binding_id,
            cursor,
            session_durable_id: row_str(&row, "session_durable_id"),
            ui_request_id: row_str(&row, "ui_request_id"),
            ui_request_kind: row_opt_str(&row, "ui_request_kind"),
            claimed_at: now,
            taken_over: prior,
        }))
    })
}

pub fn release_answer(ctx: &StoreContext, claim: &AnswerClaimRef) -> Result<bool, StoreError> {
    transaction(ctx, "release_answer", || {
        let changed = ctx.run(
            "UPDATE outbox SET question_state = 'pending', answer = NULL, answered_at = NULL, answered_by = NULL, answer_state = NULL WHERE reply_token = ? AND answered_at = ? AND answer_state = 'in_flight'",
            &[v_str(&claim.reply_token), v_i64(claim.claimed_at)],
        )?;
        Ok(changed > 0)
    })
}

pub fn confirm_answer(ctx: &StoreContext, delivered: &AnswerDelivered) -> Result<bool, StoreError> {
    transaction(ctx, "confirm_answer", || {
        let changed = ctx.run(
            "UPDATE outbox SET answer_state = 'delivered' WHERE reply_token = ? AND answer = ?",
            &[v_str(&delivered.reply_token), v_str(&delivered.answer)],
        )?;
        Ok(changed > 0)
    })
}

/// The session closed the question itself: its row is settled with the given prior answer.
pub fn mark_prior_delivered(ctx: &StoreContext, claim: &AnswerClaimRef, prior: &PriorAnswer) -> Result<bool, StoreError> {
    transaction(ctx, "mark_prior_delivered", || {
        let changed = ctx.run(
            "UPDATE outbox SET question_state = 'answered', answer = ?, answered_at = ?, answer_state = 'delivered' WHERE reply_token = ?",
            &[v_opt_str(prior.answer.clone()), v_i64(prior.answered_at), v_str(&claim.reply_token)],
        )?;
        Ok(changed > 0)
    })
}

/// Questions this session closed locally, fenced by its durable identity.
pub fn close_question(ctx: &StoreContext, now: i64, session_durable_id: &str, ui_request_id: &str) -> Result<i64, StoreError> {
    transaction(ctx, "close_question", || {
        let changed = ctx.run(
            "UPDATE outbox SET question_state = 'expired', answered_at = ? WHERE session_durable_id = ? AND ui_request_id = ? AND question_state = 'pending'",
            &[v_i64(now), v_str(session_durable_id), v_str(ui_request_id)],
        )?;
        Ok(changed as i64)
    })
}

/// The session's published control endpoint, fenced by its incarnation.
pub fn register_incarnation(ctx: &StoreContext, durable_id: &str, incarnation: &str, endpoint_socket: Option<&str>, endpoint_kind: Option<&str>) -> Result<(), StoreError> {
    transaction(ctx, "register_incarnation", || {
        ctx.run(
            "INSERT INTO session_meta (durable_id, next_seq) VALUES (?, 1) ON CONFLICT(durable_id) DO NOTHING",
            &[v_str(durable_id)],
        )?;
        ctx.run(
            "UPDATE session_meta SET incarnation = ?, endpoint_socket = ?, endpoint_kind = ? WHERE durable_id = ?",
            &[v_str(incarnation), v_opt_str(endpoint_socket.map(str::to_owned)), v_opt_str(endpoint_kind.map(str::to_owned)), v_str(durable_id)],
        )?;
        Ok(())
    })
}

pub fn clear_endpoint(ctx: &StoreContext, durable_id: &str, incarnation: &str) -> Result<(), StoreError> {
    transaction(ctx, "clear_endpoint", || {
        ctx.run(
            "UPDATE session_meta SET endpoint_socket = NULL, endpoint_kind = NULL WHERE durable_id = ? AND incarnation = ?",
            &[v_str(durable_id), v_str(incarnation)],
        )?;
        Ok(())
    })
}

/// The endpoint that currently serves a session, when one published it.
pub fn session_endpoint(ctx: &StoreContext, durable_id: &str) -> Result<Option<(String, String, String)>, StoreError> {
    Ok(ctx.one("SELECT incarnation, endpoint_socket, endpoint_kind FROM session_meta WHERE durable_id = ?", &[v_str(durable_id)])?.and_then(|row| {
        Some((row_opt_str(&row, "incarnation")?, row_opt_str(&row, "endpoint_socket")?, row_opt_str(&row, "endpoint_kind")?))
    }))
}

/// Parses a binding expiry column into epoch millis.
pub fn binding_expiry_millis(text: &str) -> Option<i64> {
    parse_rfc3339_millis(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context() -> StoreContext {
        let ctx = StoreContext::new(rusqlite::Connection::open_in_memory().expect("memory database"), StoreContext::test_config(), StoreContext::test_identity());
        for step in super::super::schema::GATEWAY_MIGRATIONS {
            for statement in *step {
                ctx.exec(statement).expect("migration step");
            }
        }
        ctx
    }

    fn input() -> BindInput {
        BindInput {
            platform: "custom".into(),
            account_id: "connector".into(),
            chat_id: "chat".into(),
            thread_id: None,
            root_message_id: None,
            progress_message_id: None,
            session_durable_id: "receiver".into(),
            direction: None,
            inbound_mode: None,
            outbound_events: None,
            policy_id: None,
            ttl_seconds: None,
        }
    }

    #[test]
    fn a_bind_defaults_its_thread_directions_and_ttl() {
        let ctx = context();
        let outcome = bind_thread(&ctx, 1_000, &input()).expect("bind");
        let RelayOutcome::Ok { value } = outcome else {
            panic!("expected ok, got {outcome:?}");
        };
        assert_eq!(value.thread_id, "@chat");
        assert_eq!(value.revision, 1);
        assert!(value.direction.inbound && value.direction.outbound);
        assert_eq!(value.inbound_mode, "auto");
        assert_eq!(value.ttl_seconds, Some(BINDING_DEFAULT_TTL_SECONDS));
    }

    #[test]
    fn a_second_session_cannot_take_an_active_thread() {
        let ctx = context();
        let _ = bind_thread(&ctx, 1_000, &input()).expect("bind");
        let mut other = input();
        other.session_durable_id = "other".into();
        let outcome = bind_thread(&ctx, 1_000, &other).expect("bind");
        assert!(matches!(outcome, RelayOutcome::Refused { code, .. } if code.as_str() == "binding_conflict"));
    }

    #[test]
    fn an_expired_binding_stops_accepting_binds_and_reports_its_status() {
        let ctx = context();
        let mut short = input();
        short.ttl_seconds = Some(1);
        let _ = bind_thread(&ctx, 0, &short).expect("bind");
        let listed = list_bindings(&ctx, 5_000, &BindingsFilter::default(), None, None).expect("list");
        let RelayOutcome::Ok { value } = listed else {
            panic!("expected ok");
        };
        assert_eq!(value.0.first().map(|binding| binding.status), Some(BindingStatus::Expired));
        let rebind = bind_thread(&ctx, 5_000, &input()).expect("bind");
        assert!(matches!(rebind, RelayOutcome::Ok { .. }));
    }

    #[test]
    fn a_report_writes_an_outbox_row_and_a_question_gets_a_reply_token() {
        let ctx = context();
        let RelayOutcome::Ok { value: binding } = bind_thread(&ctx, 1_000, &input()).expect("bind") else {
            panic!("expected ok");
        };
        let request = ReportOpRequest {
            session_durable_id: "receiver".into(),
            binding_id: Some(binding.binding_id.clone()),
            origin_delivery_ids: vec![],
            origin_local_input: false,
            event: "question".into(),
            text: "Continue?".into(),
            ui_request_id: Some("ui-1".into()),
            ui_request_kind: Some("question".into()),
        };
        let reported = report_event(&ctx, 1_000, &request).expect("report");
        let RelayOutcome::Ok { value } = reported else {
            panic!("expected ok");
        };
        assert!(value.reply_token.is_some());
        let page = read_outbox(&ctx, 1_000, &binding.binding_id, None, None, false).expect("outbox");
        let RelayOutcome::Ok { value: page } = page else {
            panic!("expected ok");
        };
        assert_eq!(page.rows.len(), 1);
        assert_eq!(page.rows[0].text, "Continue?");
        assert_eq!(page.rows[0].question_state.as_deref(), Some("pending"));
    }

    #[test]
    fn a_claimed_answer_is_released_or_delivered() {
        let ctx = context();
        let RelayOutcome::Ok { value: binding } = bind_thread(&ctx, 1_000, &input()).expect("bind") else {
            panic!("expected ok");
        };
        let request = ReportOpRequest {
            session_durable_id: "receiver".into(),
            binding_id: Some(binding.binding_id.clone()),
            origin_delivery_ids: vec![],
            origin_local_input: false,
            event: "question".into(),
            text: "Continue?".into(),
            ui_request_id: Some("ui-1".into()),
            ui_request_kind: Some("question".into()),
        };
        let RelayOutcome::Ok { value: reported } = report_event(&ctx, 1_000, &request).expect("report") else {
            panic!("expected ok");
        };
        let token = reported.reply_token.expect("token");
        let RelayOutcome::Ok { value: claim } = claim_answer(&ctx, 2_000, &binding.binding_id, &token, "yes", None).expect("claim") else {
            panic!("expected ok");
        };
        let own = AnswerClaimRef { reply_token: token.clone(), claimed_at: claim.claimed_at };
        assert!(release_answer(&ctx, &own).expect("release"));
        let RelayOutcome::Ok { value: second } = claim_answer(&ctx, 3_000, &binding.binding_id, &token, "yes", None).expect("claim") else {
            panic!("expected ok");
        };
        assert!(confirm_answer(&ctx, &AnswerDelivered { reply_token: token.clone(), claimed_at: second.claimed_at, answer: "yes".into(), answered_by: None }).expect("confirm"));
        let third = claim_answer(&ctx, 4_000, &binding.binding_id, &token, "yes", None).expect("claim");
        assert!(matches!(third, RelayOutcome::Refused { code, .. } if code.as_str() == "already_answered"));
    }

    #[test]
    fn a_cursor_round_trips() {
        let cursor = encode_cursor(7, "bind-1");
        assert_eq!(decode_cursor(&cursor), Some((7, "bind-1".to_owned())));
        assert_eq!(decode_cursor("nope"), None);
    }
}
