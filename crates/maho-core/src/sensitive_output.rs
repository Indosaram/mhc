//! Port of senpi packages/coding-agent/src/core/sensitive-output.ts.

use std::sync::OnceLock;

use regex::Regex;

fn env_assignment() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"([A-Z][A-Z0-9_]*(?:API_KEY|SECRET|TOKEN|PASSWORD|AUTH)[A-Z0-9_]*)=([^\s'"]+)"#)
            .expect("env assignment regex")
    })
}

fn authorization_bearer() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r#"(?i)(Authorization:\s*Bearer\s+)([^\s'"]+)"#).expect("authorization bearer regex")
    })
}

fn bearer_sk() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r#"(Bearer\s+)(sk-[A-Za-z0-9._-]+)"#).expect("bearer sk regex"))
}

fn github_pat() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"github_pat_[A-Za-z0-9_]{20,}").expect("github pat regex"))
}

fn ghp() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"ghp_[A-Za-z0-9]{20,}").expect("ghp regex"))
}

pub fn redact_sensitive_output(text: &str) -> String {
    let redacted = env_assignment().replace_all(text, "$1=[REDACTED]").into_owned();
    let redacted = authorization_bearer().replace_all(&redacted, "$1[REDACTED]").into_owned();
    let redacted = bearer_sk().replace_all(&redacted, "$1[REDACTED]").into_owned();
    redact_sensitive_token_values(&redacted, "[REDACTED]")
}

pub fn redact_sensitive_token_values(text: &str, replacement: &str) -> String {
    let text = ghp().replace_all(text, replacement).into_owned();
    github_pat().replace_all(&text, replacement).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_env_assignments() {
        assert_eq!(redact_sensitive_output("OPENAI_API_KEY=sk-live-abc"), "OPENAI_API_KEY=[REDACTED]");
        assert_eq!(redact_sensitive_output("GITHUB_TOKEN=xyz"), "GITHUB_TOKEN=[REDACTED]");
    }

    #[test]
    fn redacts_authorization_bearer_headers() {
        assert_eq!(redact_sensitive_output("Authorization: Bearer abc.def"), "Authorization: Bearer [REDACTED]");
        assert_eq!(redact_sensitive_output("authorization: bearer abc"), "authorization: bearer [REDACTED]");
    }

    #[test]
    fn redacts_bearer_sk_tokens() {
        assert_eq!(redact_sensitive_output("Bearer sk-1234567890"), "Bearer [REDACTED]");
    }

    #[test]
    fn redacts_github_tokens() {
        assert_eq!(redact_sensitive_output("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ"), "[REDACTED]");
        assert_eq!(redact_sensitive_output("github_pat_abcdefghijklmnopqrstuv"), "[REDACTED]");
    }

    #[test]
    fn leaves_ordinary_text_untouched() {
        assert_eq!(redact_sensitive_output("just a normal line"), "just a normal line");
    }

    #[test]
    fn a_custom_replacement_is_honored() {
        assert_eq!(redact_sensitive_token_values("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ", "<gone>"), "<gone>");
    }
}
