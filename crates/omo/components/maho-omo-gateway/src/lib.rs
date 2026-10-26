//! Native gateway component crate.
//!
//! Upstream counterpart: `packages/omo-senpi/src/components/gateway`.
//!
//! Operating-rules injection for gateway sessions. The scope lead and every session
//! with an active binding get their scope's compiled behavioral rules as one
//! `<operating-rules version="...">` block in the system prompt; a rules commit appends
//! exactly one `rules_changed` delivery per affected session.
//!
//! The `thread` tree is the session gateway: durable deliveries between sessions with sender
//! provenance, the relay that binds a session to a conversation thread, and the store extensions a
//! session may call (`packages/omo-senpi/src/components/thread/`).

#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

pub mod index;
pub mod migrations;
pub mod paths;
pub mod prompt;
pub mod rules_block;
pub mod store_extension;
pub mod thread;

pub use index::{has_scopes, GatewayComponent, GatewayComponentOptions};
pub use migrations::{GATEWAY_RULES_EXTENSION_NAME, GATEWAY_RULES_MIGRATIONS};
pub use paths::{
    gateway_database_path, gateway_inbox_directory, gateway_outbox_marker_path,
    gateway_root_directory,
};
pub use prompt::{
    GatewayRulesPromptHandler, GatewayRulesPromptOptions, GatewayRulesPromptResult,
    GatewayRulesStore, SqliteGatewayRulesStore,
};
pub use rules_block::{
    compose_gateway_rules_block, escape_rule_text, mark_gateway_rules_block,
    render_operating_rules_block, GATEWAY_RULES_SENTINEL_BEGIN, GATEWAY_RULES_SENTINEL_END,
};
pub use store_extension::{
    apply_gateway_rules_migrations, apply_gateway_rules_migrations_with_now, block_for_session, rules_committed, sessions_with_rules,
    CommittedOutcome, CommittedResult, DeliverySummary, EnqueueDeliveryRequest,
    GatewayDeliveryEnqueueSeam, RulesCommittedArgs, RulesCommittedTarget, SessionRulesBlock,
    SessionWithRule, SessionsWithRulesResult,
};
