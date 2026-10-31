//! Port of senpi packages/coding-agent/src/core/sensitive-output.ts.

use std::sync::OnceLock;

use regex::Regex;

/// An environment assignment whose name contains a credential word. One class up to the `=`, anchored at a
/// word start; the name test runs once per assignment on that name only (senpi#4283).
fn env_assignment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"\b([A-Z][A-Z0-9_]*)=([^\s'"]+)"#).expect("env assignment regex")
    })
}

fn env_secret_word() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"API_KEY|SECRET|TOKEN|PASSWORD|AUTH").expect("env secret word regex")
    })
}

fn authorization_bearer() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)\b(Authorization:\s*Bearer\s+)([^\s'"]+)"#)
            .expect("authorization bearer regex")
    })
}

fn bearer_sk() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"\b(Bearer\s+)(sk-[A-Za-z0-9._-]+)"#).expect("bearer sk regex"))
}

const REDACTED: &str = "[REDACTED]";

pub fn redact_sensitive_output(text: &str) -> String {
    let redacted = env_assignment().replace_all(text, |captures: &regex::Captures<'_>| {
        let name = &captures[1];
        if env_secret_word().is_match(&name[1..]) {
            format!("{name}={REDACTED}")
        } else {
            captures[0].to_owned()
        }
    });
    let redacted = authorization_bearer().replace_all(&redacted, "${1}[REDACTED]");
    let redacted = bearer_sk().replace_all(&redacted, "${1}[REDACTED]");
    redact_sensitive_token_values(&redacted, REDACTED)
}

/// Resolved config reference values first, then pi-ai's credential-shape scanner (known key prefixes and JWTs).
pub fn redact_sensitive_token_values(text: &str, replacement: &str) -> String {
    maho_ai::utils::secret_redaction::redact_credential_shapes_with(
        &crate::config_reference_redaction::redact_resolved_reference_values(text, replacement),
        replacement,
    )
}

/// pi-ai's credential-shape scanner, re-exported so maho-core and omo callers share one implementation.
pub use maho_ai::utils::secret_redaction::redact_credential_shapes_with as redact_credential_shapes;

/// Every sink of provider error text uses this: the session's own rules (environment assignments, resolved config
/// references) and pi-ai's value-based credential redaction, the same rules the provider stream applies.
pub fn redact_secrets(text: &str, replacement: &str) -> String {
    let session = redact_sensitive_output(text);
    let marked = if replacement == REDACTED {
        session
    } else {
        session.replace(REDACTED, replacement)
    };
    maho_ai::utils::secret_redaction::redact_secrets_with(
        &redact_sensitive_token_values(&marked, replacement),
        replacement,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_env_assignments() {
        assert_eq!(
            redact_sensitive_output("OPENAI_API_KEY=sk-live-abc"),
            "OPENAI_API_KEY=[REDACTED]"
        );
        assert_eq!(
            redact_sensitive_output("GITHUB_TOKEN=xyz"),
            "GITHUB_TOKEN=[REDACTED]"
        );
    }

    #[test]
    fn redacts_authorization_bearer_headers() {
        assert_eq!(
            redact_sensitive_output("Authorization: Bearer abc.def"),
            "Authorization: Bearer [REDACTED]"
        );
        assert_eq!(
            redact_sensitive_output("authorization: bearer abc"),
            "authorization: bearer [REDACTED]"
        );
    }

    #[test]
    fn redacts_bearer_sk_tokens() {
        assert_eq!(
            redact_sensitive_output("Bearer sk-1234567890"),
            "Bearer [REDACTED]"
        );
    }

    #[test]
    fn redacts_github_tokens() {
        assert_eq!(
            redact_sensitive_output("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ"),
            "[REDACTED]"
        );
        assert_eq!(
            redact_sensitive_output("github_pat_abcdefghijklmnopqrstuv"),
            "[REDACTED]"
        );
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        assert_eq!(
            redact_sensitive_output("just a normal line"),
            "just a normal line"
        );
    }

    #[test]
    fn credential_shapes_and_resolved_references_are_redacted() {
        crate::config_reference_redaction::remember_resolved_reference_value(
            "core-rest-04-resolved-value-1234",
        );
        assert_eq!(
            redact_sensitive_token_values(
                "key sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123 and core-rest-04-resolved-value-1234",
                "<gone>"
            ),
            "key <gone> and <gone>"
        );
    }

    #[test]
    fn an_environment_assignment_past_the_old_name_window_is_redacted() {
        let name = format!("{}_TOKEN", "A".repeat(130));
        assert_eq!(
            redact_sensitive_output(&format!("{name}=short")),
            format!("{name}=[REDACTED]")
        );
        assert_eq!(
            redact_sensitive_output("TOKEN_COUNT=3 PATH=/bin"),
            "TOKEN_COUNT=3 PATH=/bin"
        );
    }

    #[test]
    fn redact_secrets_applies_session_rules_and_provider_value_redaction() {
        assert_eq!(
            redact_secrets("MY_API_KEY=abc password: \"Hunter2!xyz9\", next", "<x>"),
            "MY_API_KEY=<x> password: \"<x>\", next"
        );
        assert_eq!(
            redact_sensitive_output("password: \"Hunter2!xyz9\""),
            "password: \"Hunter2!xyz9\"",
            "value-based provider redaction belongs to redact_secrets only"
        );
    }

    #[test]
    fn a_custom_replacement_is_honored() {
        assert_eq!(
            redact_sensitive_token_values("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ", "<gone>"),
            "<gone>"
        );
    }

    #[test]
    fn env_assignment_needs_a_credential_word_after_the_first_letter() {
        assert_eq!(
            redact_sensitive_output("MY_SECRET_VALUE=abc"),
            "MY_SECRET_VALUE=[REDACTED]"
        );
        assert_eq!(redact_sensitive_output("PATH=/usr/bin"), "PATH=/usr/bin");
        assert_eq!(redact_sensitive_output("SECRET=abc"), "SECRET=abc");
        assert_eq!(redact_sensitive_output("XSECRET=abc"), "XSECRET=[REDACTED]");
    }

    #[test]
    fn long_env_name_stays_linear() {
        let text = format!("{}=value", "A".repeat(200_000));
        assert_eq!(redact_sensitive_output(&text), text);
    }

    #[test]
    fn redacts_credential_shapes_beyond_github_tokens() {
        let r = |t: &str| redact_credential_shapes(t, "[X]");
        assert_eq!(r("key sk-abcdefgh12345 end"), "key [X] end");
        assert_eq!(r("id AKIAABCDEFGHIJKLMNOP."), "id [X].");
        assert_eq!(r("AKIAABCDEFGHIJKLMNOPQ"), "AKIAABCDEFGHIJKLMNOPQ");
        assert_eq!(r("gho_ABCDEFGHIJKLMNOPQRSTUV"), "[X]");
        assert_eq!(r("xoxb-1234567890-abc"), "[X]");
        assert_eq!(r("AIza0123456789012345678901234567890"), "[X]");
        assert_eq!(r("eyJhbGciOiJIUzI1.eyJzdWIxMjM.c2lnbmF0dXJl"), "[X]");
    }

    #[test]
    fn shapes_only_match_at_a_word_start() {
        let r = |t: &str| redact_credential_shapes(t, "[X]");
        assert_eq!(r("disk-abcdefghijkl"), "disk-abcdefghijkl");
        assert_eq!(r("https://h/path/sk-abcdefgh1234"), "https://h/path/[X]");
        assert_eq!(r("sk-ab"), "sk-ab");
    }

    #[test]
    fn percent_encoded_prefix_is_read_as_what_it_encodes() {
        let r = |t: &str| redact_credential_shapes(t, "[X]");
        assert_eq!(r("x%3Dsk-abcdefgh1234"), "x%3D[X]");
        assert_eq!(r("a=%2"), "a=%2");
    }

    #[test]
    fn non_ascii_text_survives_redaction() {
        let r = |t: &str| redact_credential_shapes(t, "[X]");
        assert_eq!(
            r("caf\u{e9} \u{1f600} sk-abcdefgh1234 \u{1f600}"),
            "caf\u{e9} \u{1f600} [X] \u{1f600}"
        );
    }
}
