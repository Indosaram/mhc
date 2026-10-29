use std::collections::HashMap;
use std::sync::{LazyLock, RwLock};

use crate::logger::log;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecentPromptDispatch {
    pub source: String,
    pub expires_at: u64,
}

static RECENT_PROMPT_DISPATCHES: LazyLock<RwLock<HashMap<String, RecentPromptDispatch>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

pub fn recent_dispatch_key(session_id: &str, dedupe_key: &str) -> String {
    format!("{session_id}\0{dedupe_key}")
}

fn prune_recent_prompt_dispatches(now: u64) {
    let mut dispatches = RECENT_PROMPT_DISPATCHES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    dispatches.retain(|_, dispatch| dispatch.expires_at > now);
}

pub fn get_recent_prompt_dispatch(
    session_id: &str,
    dedupe_key: &str,
    now: u64,
) -> Option<RecentPromptDispatch> {
    prune_recent_prompt_dispatches(now);
    let dispatches = RECENT_PROMPT_DISPATCHES
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    dispatches
        .get(&recent_dispatch_key(session_id, dedupe_key))
        .cloned()
}

pub fn remember_recent_prompt_dispatch(
    session_id: &str,
    dedupe_key: &str,
    source: &str,
    hold_ms: u64,
    now: u64,
) {
    prune_recent_prompt_dispatches(now);
    if hold_ms == 0 {
        return;
    }

    let mut dispatches = RECENT_PROMPT_DISPATCHES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    dispatches.insert(
        recent_dispatch_key(session_id, dedupe_key),
        RecentPromptDispatch {
            source: source.to_string(),
            expires_at: now.saturating_add(hold_ms),
        },
    );
    log(
        "[prompt-async-gate] remembered semantic prompt dispatch",
        Some(&serde_json::json!({
            "sessionID": session_id,
            "source": source,
            "holdMs": hold_ms,
        })),
    );
}

pub fn delete_recent_prompt_dispatch(session_id: &str, dedupe_key: &str) {
    let mut dispatches = RECENT_PROMPT_DISPATCHES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    dispatches.remove(&recent_dispatch_key(session_id, dedupe_key));
}

pub fn clear_recent_prompt_dispatches_for_testing() {
    let mut dispatches = RECENT_PROMPT_DISPATCHES
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    dispatches.clear();
}
