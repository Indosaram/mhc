//! Port of senpi packages/ai/src/api/openai-codex-responses/fallback-state.ts.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// `CODEX_WEBSOCKET_FALLBACK_COOLDOWN_MS`.
pub const CODEX_WEBSOCKET_FALLBACK_COOLDOWN_MS: u64 = 60_000;

/// `ChatGptSubscriptionWebSocketDebugStats`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptSubscriptionWebSocketDebugStats {
    pub requests: u64,
    pub connections_created: u64,
    pub connections_reused: u64,
    pub cached_context_requests: u64,
    pub store_true_requests: u64,
    pub full_context_requests: u64,
    pub delta_requests: u64,
    pub last_input_items: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_delta_input_items: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_previous_response_id: Option<String>,
    pub websocket_failures: u64,
    pub sse_fallbacks: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub websocket_fallback_active: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_web_socket_error: Option<String>,
}

static WEBSOCKET_DEBUG_STATS: LazyLock<Mutex<HashMap<String, ChatGptSubscriptionWebSocketDebugStats>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static WEBSOCKET_SSE_FALLBACK_UNTIL: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_millis() as u64).unwrap_or(0)
}

/// `getOrCreateWebSocketDebugStats`.
pub fn get_or_create_web_socket_debug_stats(session_id: &str) -> ChatGptSubscriptionWebSocketDebugStats {
    let mut stats = WEBSOCKET_DEBUG_STATS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    stats.entry(session_id.to_owned()).or_default().clone()
}

/// `getWebSocketDebugStats`.
pub fn get_web_socket_debug_stats(session_id: &str) -> Option<ChatGptSubscriptionWebSocketDebugStats> {
    WEBSOCKET_DEBUG_STATS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(session_id)
        .cloned()
}

/// `clearWebSocketFallbackState`.
pub fn clear_web_socket_fallback_state(session_id: Option<&str>) {
    let mut stats = WEBSOCKET_DEBUG_STATS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut fallback = WEBSOCKET_SSE_FALLBACK_UNTIL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    match session_id {
        Some(session_id) => {
            stats.remove(session_id);
            fallback.remove(session_id);
        }
        None => {
            stats.clear();
            fallback.clear();
        }
    }
}

/// `isWebSocketSseFallbackActive`.
pub fn is_web_socket_sse_fallback_active(session_id: Option<&str>) -> bool {
    let Some(session_id) = session_id else { return false };
    let mut fallback = WEBSOCKET_SSE_FALLBACK_UNTIL.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(fallback_until) = fallback.get(session_id).copied() else { return false };
    let mut stats = WEBSOCKET_DEBUG_STATS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if fallback_until > now_ms() {
        if let Some(stats) = stats.get_mut(session_id) {
            stats.websocket_fallback_active = Some(true);
        }
        return true;
    }
    fallback.remove(session_id);
    if let Some(stats) = stats.get_mut(session_id) {
        stats.websocket_fallback_active = Some(false);
    }
    false
}

/// `recordWebSocketSseFallback`.
pub fn record_web_socket_sse_fallback(session_id: Option<&str>) {
    let Some(session_id) = session_id else { return };
    let mut stats = WEBSOCKET_DEBUG_STATS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = stats.entry(session_id.to_owned()).or_default();
    entry.sse_fallbacks += 1;
    entry.websocket_fallback_active = Some(true);
}

/// `recordWebSocketFailure`.
pub fn record_web_socket_failure(session_id: Option<&str>, error: &crate::utils::diagnostics::Thrown) {
    let Some(session_id) = session_id else { return };
    WEBSOCKET_SSE_FALLBACK_UNTIL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(session_id.to_owned(), now_ms() + CODEX_WEBSOCKET_FALLBACK_COOLDOWN_MS);
    let mut stats = WEBSOCKET_DEBUG_STATS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = stats.entry(session_id.to_owned()).or_default();
    entry.websocket_failures += 1;
    entry.last_web_socket_error = Some(crate::utils::diagnostics::format_thrown_value(error));
    entry.websocket_fallback_active = Some(true);
}
