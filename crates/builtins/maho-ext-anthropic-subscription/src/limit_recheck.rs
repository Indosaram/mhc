//! Port of senpi `anthropic-subscription/limit-recheck.ts`.

use std::{
    collections::{HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{Arc, LazyLock, Mutex, PoisonError},
};

use maho_ai::types::{Context, Message};
use maho_core::subscription_limits::limit_store::subscription_limits;

use crate::{accounts::AccountSlot, auth_lane::PROVIDER_ID};

pub type RecheckedAccounts = Arc<Mutex<HashSet<String>>>;

struct SessionTurn {
    agent_dir: PathBuf,
    session: String,
    message: (usize, i64),
    accounts: RecheckedAccounts,
}

const MAX_SESSIONS: usize = 256;
static TURNS: LazyLock<Mutex<VecDeque<SessionTurn>>> = LazyLock::new(Default::default);

/// Native main requests do not carry the host's person-turn identity. Use the latest user
/// message's ordinal and timestamp, stable over tool continuations and retries of that message.
pub(crate) fn accounts_for_turn(
    agent_dir: &Path,
    session: Option<&str>,
    context: &Context,
) -> Option<RecheckedAccounts> {
    let session = session?;
    let users = context.messages.iter().filter_map(|message| match message {
        Message::User(user) => Some(user.timestamp),
        _ => None,
    });
    let message = users.enumerate().last()?;
    let mut turns = TURNS.lock().unwrap_or_else(PoisonError::into_inner);
    let existing = turns
        .iter()
        .position(|turn| turn.agent_dir == agent_dir && turn.session == session)
        .and_then(|index| turns.remove(index))
        .filter(|turn| turn.message == message);
    let turn = existing.unwrap_or_else(|| SessionTurn {
        agent_dir: agent_dir.to_owned(),
        session: session.to_owned(),
        message,
        accounts: RecheckedAccounts::default(),
    });
    let accounts = turn.accounts.clone();
    turns.push_back(turn);
    if turns.len() > MAX_SESSIONS {
        turns.pop_front();
    }
    Some(accounts)
}

/// Only an account-wide rate-limit block with no newer successful usage reading can be rechecked.
pub(crate) fn recheck_selection(
    accounts: &[AccountSlot],
    attempted: &HashSet<String>,
    now: f64,
) -> Vec<AccountSlot> {
    let evidence = subscription_limits().snapshot();
    accounts
        .iter()
        .cloned()
        .map(|mut account| {
            let newer_usage = account.blocked_at.is_some_and(|blocked_at| {
                evidence.iter().any(|entry| {
                    entry.provider == PROVIDER_ID
                        && entry.account == account.name
                        && entry.error.is_none()
                        && !entry.windows.is_empty()
                        && chrono::DateTime::parse_from_rfc3339(&entry.reported_at)
                            .is_ok_and(|at| at.timestamp_millis() as f64 > blocked_at)
                })
            });
            if !attempted.contains(&account.name)
                && account.block_reason.as_deref() == Some("rate_limit")
                && account.blocked_until.is_some_and(|until| until > now)
                && !newer_usage
            {
                account.block_reason = None;
                account.blocked_until = None;
                account.blocked_at = None;
            }
            account
        })
        .collect()
}
