//! Port of senpi `packages/coding-agent/src/core/retry-fallback/transient-network.ts`.

use std::sync::LazyLock;

use maho_ai::utils::retry::strip_provider_request_ids;
use regex::Regex;

/// Transport blips (timeouts, DNS, socket drops, a cut stream); shared by the cooldown and the fallback
/// decision.
static TRANSPORT_FAILURE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"timed? out|timeout|econnreset|econnrefused|etimedout|socket hang up|socket connection was closed|network.?error|connection.?error|connection.?refused|connection.?lost|other side closed|fetch failed|getaddrinfo|enotfound|eai_again|upstream.?connect|reset before headers|terminated|websocket.?closed|websocket.?error|ended without|stream ended before message_stop|stream ended before a terminal response event|http2 request did not get a response|\btls\b|\bssl\b|econnaborted|epipe")
        .unwrap_or_else(|e| panic!("transport failure: {e}"))
});

/// A limit, spent account, loaded model or server-side failure (5xx, "unavailable") is the provider's
/// answer, even beside a timeout, matching the precedence in cooldown.rs.
static PROVIDER_VERDICT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"usage[- ]limit|quota|billing|credits|rate[ -]?limit|\b429\b|too many requests|overloaded|capacity|forbidden|resets? (?:at|in)\b|\b5\d\d\b|server|unavailable|high demand|overflow")
        .unwrap_or_else(|e| panic!("provider verdict: {e}"))
});

fn normalize(error_message: &str) -> String {
    strip_provider_request_ids(error_message).to_lowercase()
}

pub fn is_transport_failure_text(normalized_message: &str) -> bool {
    TRANSPORT_FAILURE.is_match(normalized_message)
}

pub fn is_pure_network_transient(error_message: Option<&str>) -> bool {
    let Some(error_message) = error_message else {
        return false;
    };
    let message = normalize(error_message);
    is_transport_failure_text(&message) && !PROVIDER_VERDICT.is_match(&message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_blips_are_pure_network_transients() {
        for text in [
            "Request timed out",
            "ECONNRESET",
            "fetch failed",
            "socket hang up",
            "TLS handshake failed",
        ] {
            assert!(is_pure_network_transient(Some(text)), "{text}");
        }
    }

    #[test]
    fn a_provider_verdict_wins_over_a_timeout() {
        for text in [
            "timeout: 429 too many requests",
            "503 service unavailable after timeout",
            "usage limit reached, connection lost",
        ] {
            assert!(!is_pure_network_transient(Some(text)), "{text}");
        }
        assert!(!is_pure_network_transient(Some("invalid api key")));
        assert!(!is_pure_network_transient(None));
    }
}
