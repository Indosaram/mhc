//! Gateway store schema.
//!
//! Upstream counterpart: `thread/gateway/schema.ts`. Steps are append-only; a store rewound to an
//! older version replays them, so every step is idempotent where it touches surviving objects.

use super::provenance_record::{PROVENANCE_OBJECT, PROVENANCE_TRIGGER};

/// A store whose schema is newer than this binary refuses to open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewaySchemaVersionError {
    pub found: i64,
    pub supported: i64,
    pub scope: String,
}

impl GatewaySchemaVersionError {
    pub fn new(found: i64, supported: i64, scope: impl Into<String>) -> Self {
        Self { found, supported, scope: scope.into() }
    }

    pub fn code(&self) -> &'static str {
        "gateway_schema_too_new"
    }
}

impl std::fmt::Display for GatewaySchemaVersionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{} schema {} is newer than this binary's supported version {}.", self.scope, self.found, self.supported)
    }
}

impl std::error::Error for GatewaySchemaVersionError {}

const DELIVERIES_TABLE: &str = r#"CREATE TABLE deliveries (
      delivery_id TEXT PRIMARY KEY,
      target_durable_id TEXT NOT NULL,
      seq INTEGER NOT NULL,
      sender TEXT NOT NULL,
      sender_turn TEXT,
      envelope TEXT NOT NULL,
      body TEXT NOT NULL,
      bytes INTEGER NOT NULL,
      mode_requested TEXT NOT NULL CHECK (mode_requested IN ('auto', 'steer', 'follow_up')),
      mode_effective TEXT CHECK (mode_effective IS NULL OR mode_effective IN ('steer', 'follow_up')),
      expected_turn_id INTEGER,
      state TEXT NOT NULL CHECK (state IN ('queued', 'admitting', 'admitted', 'applied', 'refused', 'uncertain')),
      reason TEXT,
      admitted_by TEXT,
      claimed_at INTEGER,
      attempt INTEGER NOT NULL DEFAULT 0,
      admission_kind TEXT,
      turn_epoch INTEGER,
      root_id TEXT NOT NULL,
      hop INTEGER NOT NULL,
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL,
      binding_id TEXT,
      binding_revision INTEGER,
      UNIQUE (target_durable_id, seq)
    )"#;

const RECEIPTS_TABLE: &str = r#"CREATE TABLE receipts (
      principal TEXT NOT NULL,
      operation TEXT NOT NULL,
      idempotency_key TEXT NOT NULL,
      args_hash TEXT NOT NULL,
      status TEXT NOT NULL CHECK (status IN ('prepared', 'completed', 'uncertain')),
      delivery_id TEXT,
      owner_instance TEXT NOT NULL,
      result TEXT,
      error_note TEXT,
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL,
      PRIMARY KEY (principal, operation, idempotency_key)
    )"#;

const OUTBOX_TABLE: &str = r#"CREATE TABLE outbox (
      cursor INTEGER PRIMARY KEY AUTOINCREMENT,
      binding_id TEXT NOT NULL,
      revision INTEGER NOT NULL,
      event_kind TEXT NOT NULL CHECK (event_kind IN ('milestone', 'report', 'question', 'completion', 'question_closed')),
      payload TEXT NOT NULL,
      state TEXT NOT NULL CHECK (state IN ('pending', 'acked')),
      provider_message_id TEXT,
      created_at INTEGER NOT NULL,
      acked_at INTEGER
    )"#;

const BINDINGS_V7_TABLE: &str = r#"CREATE TABLE bindings_v7 (
      seq INTEGER PRIMARY KEY AUTOINCREMENT,
      binding_id TEXT NOT NULL UNIQUE,
      schema_version INTEGER NOT NULL DEFAULT 1,
      revision INTEGER NOT NULL CHECK (revision >= 1),
      status TEXT NOT NULL CHECK (status IN ('active', 'detached', 'expired')),
      platform TEXT NOT NULL CHECK (platform IN ('discord', 'telegram', 'slack', 'notion', 'feishu', 'herdr', 'custom', 'whatsapp')),
      account_id TEXT NOT NULL,
      chat_id TEXT NOT NULL,
      thread_id TEXT NOT NULL,
      root_message_id TEXT,
      progress_message_id TEXT,
      session_realm_id TEXT NOT NULL,
      session_durable_id TEXT NOT NULL,
      direction_inbound INTEGER NOT NULL CHECK (direction_inbound IN (0, 1)),
      direction_outbound INTEGER NOT NULL CHECK (direction_outbound IN (0, 1)),
      inbound_mode TEXT NOT NULL CHECK (inbound_mode IN ('auto', 'follow_up')),
      outbound_events TEXT NOT NULL,
      policy_id TEXT NOT NULL,
      created_at TEXT NOT NULL,
      updated_at TEXT NOT NULL,
      lease_started_at TEXT NOT NULL,
      ttl_seconds INTEGER,
      expires_at TEXT,
      CHECK (direction_inbound = 1 OR direction_outbound = 1)
    )"#;

/// Every migration step, in order; `user_version` counts them.
pub static GATEWAY_MIGRATIONS: &[&[&str]] = &[
    &[
        DELIVERIES_TABLE,
        "CREATE INDEX deliveries_target_state_seq ON deliveries (target_durable_id, state, seq)",
        "CREATE INDEX deliveries_root ON deliveries (root_id)",
        "CREATE INDEX deliveries_sender_turn ON deliveries (sender, sender_turn)",
        RECEIPTS_TABLE,
        "CREATE INDEX receipts_expiry ON receipts (expires_at)",
        r#"CREATE TABLE causal_roots (
      root_id TEXT PRIMARY KEY,
      origin_principal TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      expires_at INTEGER NOT NULL
    )"#,
        r#"CREATE TABLE causal_edges (
      root_id TEXT NOT NULL,
      from_durable_id TEXT NOT NULL,
      to_durable_id TEXT NOT NULL,
      delivery_id TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      PRIMARY KEY (root_id, from_durable_id, to_durable_id)
    )"#,
        r#"CREATE TABLE rate_buckets (
      sender TEXT NOT NULL,
      target_durable_id TEXT NOT NULL,
      tokens REAL NOT NULL,
      updated_at INTEGER NOT NULL,
      PRIMARY KEY (sender, target_durable_id)
    )"#,
        r#"CREATE TABLE session_meta (
      durable_id TEXT PRIMARY KEY,
      next_seq INTEGER NOT NULL,
      applied_seq INTEGER NOT NULL DEFAULT 0
    )"#,
        // `seq` is the rowid the bindings snapshot watermarks on: AUTOINCREMENT never hands a
        // deleted binding's rowid to a later one.
        "CREATE TABLE bindings (\n      seq INTEGER PRIMARY KEY AUTOINCREMENT,\n      binding_id TEXT NOT NULL UNIQUE,\n      schema_version INTEGER NOT NULL DEFAULT 1,\n      revision INTEGER NOT NULL CHECK (revision >= 1),\n      status TEXT NOT NULL CHECK (status IN ('active', 'detached', 'expired')),\n      platform TEXT NOT NULL CHECK (platform IN ('discord', 'telegram', 'slack', 'notion', 'feishu', 'herdr', 'custom', 'whatsapp')),\n      account_id TEXT NOT NULL,\n      chat_id TEXT NOT NULL,\n      thread_id TEXT NOT NULL,\n      root_message_id TEXT,\n      progress_message_id TEXT,\n      session_realm_id TEXT NOT NULL,\n      session_durable_id TEXT NOT NULL,\n      direction_inbound INTEGER NOT NULL CHECK (direction_inbound IN (0, 1)),\n      direction_outbound INTEGER NOT NULL CHECK (direction_outbound IN (0, 1)),\n      inbound_mode TEXT NOT NULL CHECK (inbound_mode IN ('auto', 'follow_up')),\n      outbound_events TEXT NOT NULL,\n      policy_id TEXT NOT NULL,\n      created_at TEXT NOT NULL,\n      updated_at TEXT NOT NULL,\n      lease_started_at TEXT NOT NULL,\n      ttl_seconds INTEGER,\n      expires_at TEXT,\n      CHECK (direction_inbound = 1 OR direction_outbound = 1)\n    )",
        "CREATE UNIQUE INDEX bindings_one_active_thread ON bindings (platform, account_id, chat_id, thread_id) WHERE status = 'active'",
        "CREATE INDEX bindings_session ON bindings (session_durable_id, status)",
        OUTBOX_TABLE,
        "CREATE INDEX outbox_binding_cursor ON outbox (binding_id, cursor)",
        r#"CREATE TABLE gateway_meta (
      key TEXT PRIMARY KEY,
      value TEXT NOT NULL
    )"#,
    ],
    // v2: the relay side of bindings (incarnation, question/answer columns, consumer cursors, arms).
    &[
        "ALTER TABLE session_meta ADD COLUMN incarnation TEXT",
        "ALTER TABLE outbox ADD COLUMN session_durable_id TEXT",
        "ALTER TABLE outbox ADD COLUMN reply_token TEXT",
        "ALTER TABLE outbox ADD COLUMN ui_request_id TEXT",
        "ALTER TABLE outbox ADD COLUMN incarnation TEXT",
        "ALTER TABLE outbox ADD COLUMN question_state TEXT CHECK (question_state IS NULL OR question_state IN ('pending', 'answered', 'expired', 'cancelled'))",
        "ALTER TABLE outbox ADD COLUMN answer TEXT",
        "ALTER TABLE outbox ADD COLUMN answered_at INTEGER",
        "ALTER TABLE outbox ADD COLUMN outcome TEXT CHECK (outcome IS NULL OR outcome IN ('completed', 'failed', 'cancelled'))",
        "CREATE UNIQUE INDEX outbox_reply_token ON outbox (reply_token) WHERE reply_token IS NOT NULL",
        r#"CREATE TABLE outbox_cursors (
      binding_id TEXT PRIMARY KEY,
      acked_cursor INTEGER NOT NULL,
      updated_at INTEGER NOT NULL
    )"#,
        // One row per arm: a later run's arm must not replace the arm of a run that settled while its
        // completion write was still outstanding.
        r#"CREATE TABLE completion_arms (
      arm_seq INTEGER PRIMARY KEY AUTOINCREMENT,
      session_durable_id TEXT NOT NULL,
      binding_id TEXT NOT NULL,
      revision INTEGER NOT NULL,
      text TEXT NOT NULL,
      armed_at INTEGER NOT NULL
    )"#,
        "CREATE INDEX completion_arms_session ON completion_arms (session_durable_id, armed_at)",
        "INSERT OR IGNORE INTO gateway_meta (key, value) VALUES ('realm_id', 'realm-' || lower(hex(randomblob(16))))",
        "INSERT OR IGNORE INTO gateway_meta (key, value) VALUES ('token_secret', lower(hex(randomblob(32))))",
    ],
    // v3: which extension UI request a question row answers, and whether a claimed answer is in flight.
    &[
        "ALTER TABLE outbox ADD COLUMN ui_request_kind TEXT CHECK (ui_request_kind IS NULL OR ui_request_kind IN ('question', 'select', 'confirm', 'input', 'editor'))",
        "ALTER TABLE outbox ADD COLUMN answer_state TEXT CHECK (answer_state IS NULL OR answer_state IN ('in_flight', 'delivered'))",
    ],
    // v4: who answered a question.
    &["ALTER TABLE outbox ADD COLUMN answered_by TEXT"],
    // v5: the session's published control endpoint, fenced by its existing incarnation.
    &[
        "ALTER TABLE session_meta ADD COLUMN endpoint_socket TEXT",
        "ALTER TABLE session_meta ADD COLUMN endpoint_kind TEXT CHECK (endpoint_kind IS NULL OR endpoint_kind IN ('tui', 'rpc_host'))",
    ],
    // v6: the store extension registry and the connector author an extension enqueue records.
    &[
        "CREATE TABLE extension_schema (name TEXT PRIMARY KEY, version INTEGER NOT NULL, updated_at INTEGER NOT NULL)",
        "ALTER TABLE deliveries ADD COLUMN actor_user_id TEXT",
        "CREATE TABLE extension_objects (type TEXT NOT NULL, name TEXT NOT NULL, owner TEXT, PRIMARY KEY (type, name))",
        "INSERT INTO extension_objects (type, name, owner) SELECT type, name, NULL FROM sqlite_schema",
    ],
    // v7: `whatsapp` is a binding platform; SQLite cannot widen a CHECK in place, so `bindings` is rebuilt.
    // This step runs with foreign_keys OFF, and the AUTOINCREMENT high-water mark is carried over.
    &[
        BINDINGS_V7_TABLE,
        "INSERT INTO bindings_v7 SELECT seq, binding_id, schema_version, revision, status, platform, account_id, chat_id, thread_id, root_message_id, progress_message_id, session_realm_id, session_durable_id, direction_inbound, direction_outbound, inbound_mode, outbound_events, policy_id, created_at, updated_at, lease_started_at, ttl_seconds, expires_at FROM bindings",
        "UPDATE sqlite_sequence SET seq = (SELECT seq FROM sqlite_sequence WHERE name = 'bindings') WHERE name = 'bindings_v7'",
        "DROP TABLE bindings",
        "ALTER TABLE bindings_v7 RENAME TO bindings",
        "CREATE UNIQUE INDEX bindings_one_active_thread ON bindings (platform, account_id, chat_id, thread_id) WHERE status = 'active'",
        "CREATE INDEX bindings_session ON bindings (session_durable_id, status)",
    ],
    // v8: new deliveries require sender provenance; old envelopes are not rewritten.
    &[PROVENANCE_TRIGGER, PROVENANCE_OBJECT],
    // v9: who thread_create'd which session, and each extension's persisted descriptor. A store a build
    // from before the renumber migrated already sits at v8 with these tables but without the provenance
    // trigger, so this step replays v8 (idempotent) and creates only what is missing.
    &[
        PROVENANCE_TRIGGER,
        PROVENANCE_OBJECT,
        r#"CREATE TABLE IF NOT EXISTS thread_creations (
      creator_durable_id TEXT NOT NULL,
      created_durable_id TEXT NOT NULL,
      created_at INTEGER NOT NULL,
      PRIMARY KEY (creator_durable_id, created_durable_id)
    )"#,
        "CREATE TABLE IF NOT EXISTS extension_registrations (name TEXT PRIMARY KEY, descriptor_json TEXT NOT NULL, updated_at INTEGER NOT NULL)",
        r#"INSERT OR IGNORE INTO extension_objects (type, name, owner)
     SELECT s.type, s.name, NULL FROM sqlite_schema s
     WHERE NOT EXISTS (SELECT 1 FROM extension_objects e WHERE e.type = s.type AND e.name = s.name COLLATE NOCASE)"#,
    ],
    // v10: a receipts table (kept for existing stores; no longer written).
    &[
        PROVENANCE_TRIGGER,
        PROVENANCE_OBJECT,
        r#"CREATE TABLE IF NOT EXISTS toolkit_replay_receipts (
      replay_key TEXT PRIMARY KEY,
      op TEXT NOT NULL,
      args_sha256 TEXT NOT NULL,
      result_json TEXT NOT NULL,
      turn_at INTEGER NOT NULL,
      created_at INTEGER NOT NULL
    )"#,
        "CREATE INDEX IF NOT EXISTS toolkit_replay_receipts_turn_at ON toolkit_replay_receipts (turn_at)",
        r#"INSERT OR IGNORE INTO extension_objects (type, name, owner)
     SELECT s.type, s.name, NULL FROM sqlite_schema s
     WHERE NOT EXISTS (SELECT 1 FROM extension_objects e WHERE e.type = s.type AND e.name = s.name COLLATE NOCASE)"#,
    ],
];

pub const GATEWAY_TABLES: &[&str] = &[
    "deliveries",
    "receipts",
    "causal_roots",
    "causal_edges",
    "rate_buckets",
    "session_meta",
    "bindings",
    "outbox",
    "gateway_meta",
    "outbox_cursors",
    "completion_arms",
    "extension_schema",
    "extension_objects",
    "thread_creations",
    "extension_registrations",
    "toolkit_replay_receipts",
];

/// Migration indexes (0-based) whose steps must run with foreign_keys OFF.
pub const GATEWAY_NEEDS_FK_OFF: &[usize] = &[6];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_step_is_non_empty_and_the_count_is_the_supported_version() {
        assert_eq!(GATEWAY_MIGRATIONS.len(), 10);
        for (index, step) in GATEWAY_MIGRATIONS.iter().enumerate() {
            assert!(!step.is_empty(), "step {index} is empty");
        }
        assert_eq!(GATEWAY_NEEDS_FK_OFF.to_vec(), vec![6usize]);
    }

    #[test]
    fn a_newer_store_is_refused_with_its_code() {
        let error = GatewaySchemaVersionError::new(11, 10, "Gateway");
        assert_eq!(error.code(), "gateway_schema_too_new");
        assert_eq!(error.to_string(), "Gateway schema 11 is newer than this binary's supported version 10.");
    }
}
