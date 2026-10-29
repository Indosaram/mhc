use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, RwLock};

use crate::logger::log;

use super::types::{PromptAsyncReservation, PromptAsyncReservationReleaseOptions};

static PROMPT_ASYNC_RESERVATIONS: LazyLock<RwLock<HashMap<String, PromptAsyncReservation>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

type ExpiredReservationHandler = Arc<dyn Fn(&str) + Send + Sync>;
static EXPIRED_RESERVATION_HANDLER: LazyLock<RwLock<Option<ExpiredReservationHandler>>> =
    LazyLock::new(|| RwLock::new(None));

static RESERVATION_TOKEN_SEQ: AtomicU64 = AtomicU64::new(1);

pub fn next_reservation_token() -> u64 {
    RESERVATION_TOKEN_SEQ.fetch_add(1, Ordering::SeqCst)
}

pub fn set_expired_reservation_handler(handler: ExpiredReservationHandler) {
    *EXPIRED_RESERVATION_HANDLER
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handler);
}

fn notify_expired_reservation(session_id: &str) {
    let handler = EXPIRED_RESERVATION_HANDLER
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone();
    if let Some(handler_fn) = handler {
        handler_fn(session_id);
    }
}

fn prune_expired_reservations(now: u64) {
    let mut expired_session_ids = Vec::new();
    {
        let mut reservations = PROMPT_ASYNC_RESERVATIONS
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut to_remove = Vec::new();
        for (session_id, reservation) in reservations.iter() {
            if let Some(expires_at) = reservation.expires_at
                && expires_at <= now
            {
                to_remove.push((session_id.clone(), reservation.source.clone()));
            }
        }
        for (session_id, source) in to_remove {
            reservations.remove(&session_id);
            expired_session_ids.push(session_id.clone());
            log(
                "[prompt-async-gate] expired reservation released",
                Some(&serde_json::json!({
                    "sessionID": session_id,
                    "source": source,
                })),
            );
        }
    }
    for session_id in expired_session_ids {
        notify_expired_reservation(&session_id);
    }
}

pub fn get_active_reservation(session_id: &str, now: u64) -> Option<PromptAsyncReservation> {
    prune_expired_reservations(now);
    let reservations = PROMPT_ASYNC_RESERVATIONS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reservations.get(session_id).cloned()
}

pub fn get_prompt_reservation(session_id: &str) -> Option<PromptAsyncReservation> {
    let reservations = PROMPT_ASYNC_RESERVATIONS
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reservations.get(session_id).cloned()
}

pub fn set_prompt_reservation(session_id: &str, reservation: PromptAsyncReservation) {
    let mut reservations = PROMPT_ASYNC_RESERVATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reservations.insert(session_id.to_string(), reservation);
}

pub fn finish_prompt_reservation(
    session_id: &str,
    reservation: &PromptAsyncReservation,
    dispatch_attempted: bool,
    post_dispatch_hold_ms: u64,
    now: u64,
) {
    let mut reservations = PROMPT_ASYNC_RESERVATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(current) = reservations.get(session_id) else {
        return;
    };
    if current.token != reservation.token {
        return;
    }

    if dispatch_attempted && post_dispatch_hold_ms > 0 {
        let mut updated = reservation.clone();
        updated.expires_at = Some(now.saturating_add(post_dispatch_hold_ms));
        reservations.insert(session_id.to_string(), updated);
        return;
    }

    reservations.remove(session_id);
}

pub fn delete_prompt_reservation(session_id: &str) {
    let mut reservations = PROMPT_ASYNC_RESERVATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reservations.remove(session_id);
}

pub fn clear_prompt_reservations_for_testing() {
    let mut reservations = PROMPT_ASYNC_RESERVATIONS
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    reservations.clear();
}

pub const TRANSIENT_RETRY_RESERVATION_OWNER: &str = "model-suggestion-retry";

pub fn is_transient_retry_reservation_owner(reservation_source: &str) -> bool {
    reservation_source == TRANSIENT_RETRY_RESERVATION_OWNER
        || reservation_source.starts_with(&format!("{TRANSIENT_RETRY_RESERVATION_OWNER}:"))
}

pub fn reservation_source_matches(
    reservation_source: &str,
    expected_sources: &[String],
    expected_prefixes: Option<&[String]>,
    supersede_transient_retry_owners: bool,
) -> bool {
    if expected_sources.iter().any(|s| s == reservation_source) {
        return true;
    }

    if supersede_transient_retry_owners && is_transient_retry_reservation_owner(reservation_source)
    {
        return true;
    }

    let Some(prefixes) = expected_prefixes else {
        return false;
    };

    prefixes
        .iter()
        .filter(|prefix| !prefix.is_empty() && prefix.ends_with(':'))
        .any(|prefix| reservation_source.starts_with(prefix))
}

pub fn reservation_source_matches_options(
    reservation_source: &str,
    fallback_expected_source: &str,
    options: Option<&PromptAsyncReservationReleaseOptions>,
) -> bool {
    let (expected_sources, expected_prefixes, supersede) = match options {
        Some(opts) => {
            let sources = match &opts.reserved_by {
                Some(list) => list.clone(),
                None => vec![fallback_expected_source.to_string()],
            };
            (
                sources,
                opts.reserved_by_prefix.as_deref(),
                opts.supersede_transient_retry_owners,
            )
        }
        None => (vec![fallback_expected_source.to_string()], None, false),
    };

    reservation_source_matches(
        reservation_source,
        &expected_sources,
        expected_prefixes,
        supersede,
    )
}
