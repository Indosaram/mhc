//! Store migrations for the `gateway_rules` store extension.
//!
//! Upstream counterpart: `store-extension/migrations.ts`.

pub const GATEWAY_RULES_EXTENSION_NAME: &str = "gateway_rules";

pub const GATEWAY_RULES_MIGRATIONS: &[&[&str]] = &[&[
    r#"CREATE TABLE gateway_rules_blocks (
      session_durable_id TEXT PRIMARY KEY,
      scope TEXT NOT NULL,
      version TEXT NOT NULL,
      block TEXT NOT NULL,
      updated_at INTEGER NOT NULL
    )"#,
]];
