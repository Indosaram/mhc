//! Port of `test/normalize-session-id.test.ts` and the `normalizeSessionId` block of
//! `src/read-state.test.ts`.

use boulder_state::{SessionPlatform, normalize_session_id, normalize_session_id_with_platform};
use pretty_assertions::assert_eq;

#[test]
fn bare_id_defaults_to_opencode() {
    assert_eq!(normalize_session_id("sess_abc"), "opencode:sess_abc");
}

#[test]
fn bare_id_with_codex_platform_uses_codex() {
    assert_eq!(
        normalize_session_id_with_platform("sess_abc", SessionPlatform::Codex),
        "codex:sess_abc"
    );
}

#[test]
fn opencode_prefixed_id_is_unchanged() {
    assert_eq!(
        normalize_session_id("opencode:sess_abc"),
        "opencode:sess_abc"
    );
}

#[test]
fn existing_codex_prefix_wins_over_opencode_platform() {
    assert_eq!(
        normalize_session_id_with_platform("codex:sess_abc", SessionPlatform::Opencode),
        "codex:sess_abc"
    );
}

#[test]
fn empty_id_becomes_opencode_empty_id() {
    assert_eq!(normalize_session_id(""), "opencode:");
}

#[test]
fn bare_id_with_senpi_platform_uses_senpi() {
    assert_eq!(
        normalize_session_id_with_platform("abc", SessionPlatform::Senpi),
        "senpi:abc"
    );
}

#[test]
fn senpi_prefixed_id_is_unchanged() {
    assert_eq!(normalize_session_id("senpi:abc"), "senpi:abc");
}

#[test]
fn existing_senpi_prefix_wins_over_codex_platform() {
    assert_eq!(
        normalize_session_id_with_platform("senpi:abc", SessionPlatform::Codex),
        "senpi:abc"
    );
}
