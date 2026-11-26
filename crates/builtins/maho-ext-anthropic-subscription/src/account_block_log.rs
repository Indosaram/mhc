//! Port of senpi `anthropic-subscription/account-block-log.ts`.

use std::path::Path;

use serde_json::{Map, json};

use crate::accounts::AccountSlot;

pub(crate) fn log_account_blocked(
    agent_dir: &Path,
    account: &AccountSlot,
    until: Option<f64>,
    shared: bool,
) {
    // Use the native session logger's allowlisted keys. Never pass raw provider errors: they
    // can contain account identities or credentials in arbitrary formats.
    let mut data = Map::from_iter([
        (
            "actor".into(),
            json!(crate::seat_digest::seat_digest(
                &account.name,
                Some(agent_dir)
            )),
        ),
        (
            "reason".into(),
            json!(account.block_reason.as_deref().unwrap_or("model_limit")),
        ),
        (
            "source".into(),
            json!(if shared { "shared" } else { "session" }),
        ),
    ]);
    if let Some(until) = until {
        data.insert("to".into(), json!(crate::model_blocks::iso_ms(until)));
    }
    maho_core::session_log::SessionLogger::create(
        Some(&agent_dir.to_string_lossy()),
        Default::default(),
    )
    .info("account_blocked", Some(&data));
}
