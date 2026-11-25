//! Port of senpi packages/coding-agent/src/core/session-move/prepare.ts.
//!
//! The receiver's readying step (senpi#6435).

use super::move_ledger::{MoveLedger, MoveRecord};
use super::repository_check::{CheckoutVerdict, checkout_refusal, checkout_verdict};
use crate::session_move::bundle::RepositoryIdentity;

pub struct PrepareMoveOptions<'a> {
    pub ledger: &'a MoveLedger,
    pub move_id: &'a str,
    pub cwd: Option<&'a str>,
    pub repository: Option<&'a RepositoryIdentity>,
}

pub async fn prepare_move(options: PrepareMoveOptions<'_>) -> Result<MoveRecord, String> {
    let Some(cwd) = options.cwd else {
        return options
            .ledger
            .prepare(options.move_id)
            .await
            .map_err(|e| e.to_string());
    };
    let verdict = checkout_verdict(cwd, options.repository, None).await;
    if verdict != CheckoutVerdict::Same {
        return Err(checkout_refusal(verdict, cwd, None).to_string());
    }
    options
        .ledger
        .prepare_for_cwd(options.move_id, Some(cwd))
        .await
        .map_err(|e| e.to_string())
}
