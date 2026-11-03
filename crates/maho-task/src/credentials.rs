//! Credential failures of a delegated run (`docs/cleanroom/specs/task.md` Delta D2/D3): whether a
//! failed run says the child's provider cannot authenticate at all, and the remedy line added to
//! the record's error.

/// Class of a failed run's text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialFailure {
    /// Not a credential failure.
    None,
    /// A credential was sent and refused, or can no longer be renewed.
    Rejected,
    /// No credential is stored or signed in.
    Missing,
}

impl CredentialFailure {
    pub fn is_credential(self) -> bool {
        !matches!(self, Self::None)
    }
}

/// Texts that say no credential exists (always, regardless of a model name).
const MISSING: &[&str] = &[
    "no api key found for",
    "no api key for",
    "no accounts:",
    "no accounts left",
    "not logged in",
    "access token is missing",
    "missing access token",
    "access token is required",
    "access token required",
    "no access token",
];

/// Texts that say the provider requires authentication; `missing` only when no model is named.
const AUTH_REQUIRED: &[&str] = &[
    "authentication is required",
    "authentication required",
    "requires authentication",
];

/// Texts that say a credential was refused or cannot be renewed.
const REJECTED: &[&str] = &[
    "unauthorized",
    "invalid api key",
    "incorrect api key",
    "api key is invalid",
    "api key not valid",
    "invalid x-api-key",
    "oauth refresh failed",
    "invalid_grant",
    "authentication failed for",
    "token was rejected",
    "token has been revoked",
    "token was revoked",
    "subscription required",
    "subscription is required",
    "requires a subscription",
    "requires an active subscription",
];

/// Session expiry texts; `rejected` only when the sentence directly after the expiry statement
/// tells the user to run `/login` (the engine's meta "session expired ... /login" text), so a
/// stray log line or a `/login` hint elsewhere is not evidence.
const SESSION_EXPIRED: &[&str] = &["session expired", "session has expired"];

/// Words that tie a 403 to the account or its credentials.
const ACCOUNT_WORDS: &[&str] = &[
    "account",
    "credential",
    "token",
    "key",
    "organization",
    "organisation",
    "subscription",
];

/// Whether `text` (lower case) contains `code` as a standalone number.
fn has_status(text: &str, code: &str) -> bool {
    let bytes = text.as_bytes();
    text.match_indices(code).any(|(start, _)| {
        let end = start + code.len();
        let before = start == 0 || !bytes[start - 1].is_ascii_digit();
        let after = end == bytes.len() || !bytes[end].is_ascii_digit();
        before && after
    })
}

/// Whether `text` (lower case) contains `word`, or its plural with a trailing `s`, with no letter
/// or digit on either side.
fn has_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let boundary = |byte: u8| !byte.is_ascii_alphanumeric();
    text.match_indices(word).any(|(start, _)| {
        let mut end = start + word.len();
        if bytes.get(end) == Some(&b's') {
            end += 1;
        }
        (start == 0 || boundary(bytes[start - 1])) && (end == bytes.len() || boundary(bytes[end]))
    })
}

/// Whether `text` (lower case) names a model: the word model/models or the failed model id.
fn names_model(text: &str, model_id: Option<&str>) -> bool {
    has_word(text, "model")
        || model_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .is_some_and(|id| text.contains(&id.to_lowercase()))
}

fn ends_sentence(c: char) -> bool {
    matches!(c, '.' | '!' | '?' | '\n')
}

/// Whether `text` (lower case) has a session expiry statement whose directly following sentence
/// mentions `/login`.
fn expiry_followed_by_login(text: &str) -> bool {
    SESSION_EXPIRED.iter().any(|needle| {
        text.match_indices(needle).any(|(start, _)| {
            let after = &text[start + needle.len()..];
            let Some(end) = after.find(ends_sentence) else {
                return false;
            };
            let next =
                after[end..].trim_start_matches(|c: char| ends_sentence(c) || c.is_whitespace());
            let next = next.find(ends_sentence).map_or(next, |stop| &next[..stop]);
            next.contains("/login")
        })
    })
}

/// Classifies a failure `text` (record error plus log tail) of a run on `model_id` (the model
/// part after `provider/`). `rejected` wins when both classes match.
pub fn classify(text: &str, model_id: Option<&str>) -> CredentialFailure {
    let text = text.to_lowercase();
    let names_model = names_model(&text, model_id);
    let forbidden = has_status(&text, "403") || text.contains("forbidden");
    let rejected = has_status(&text, "401")
        || REJECTED.iter().any(|needle| text.contains(needle))
        || expiry_followed_by_login(&text)
        || (forbidden && !names_model && ACCOUNT_WORDS.iter().any(|word| has_word(&text, word)));
    if rejected {
        return CredentialFailure::Rejected;
    }
    let missing = MISSING.iter().any(|needle| text.contains(needle))
        || (!names_model && AUTH_REQUIRED.iter().any(|needle| text.contains(needle)));
    if missing {
        CredentialFailure::Missing
    } else {
        CredentialFailure::None
    }
}

/// `(provider, model id)` of `provider/id`; `None` without a provider part.
pub fn split_model(model: &str) -> Option<(&str, &str)> {
    let (provider, id) = model.trim().split_once('/')?;
    (!provider.is_empty() && !id.is_empty()).then_some((provider, id))
}

/// The remedy line for a credential failure of `provider`.
pub fn remedy_line(class: CredentialFailure, provider: &str) -> Option<String> {
    let first = match class {
        CredentialFailure::None => return None,
        CredentialFailure::Missing => format!(
            "Credentials missing: no credentials were found for provider `{provider}`. Sign in with `/login {provider}` in an interactive session or add the provider's API key"
        ),
        CredentialFailure::Rejected => format!(
            "Credentials rejected: the credentials of provider `{provider}` were rejected. Re-authenticate with `/login {provider}` or replace the provider's API key"
        ),
    };
    Some(format!(
        "{first}; alternatively point the category or agent at another provider in omo.jsonc."
    ))
}

/// The error of a failed run on `model` with the remedy line appended (once). `error` is the
/// record's error, `log_tail` the end of the run's stderr. `None` = keep the error unchanged.
pub fn annotate(error: Option<&str>, log_tail: &str, model: Option<&str>) -> Option<String> {
    let (provider, id) = split_model(model?)?;
    let error = error.unwrap_or_default();
    let class = classify(&format!("{error}\n{log_tail}"), Some(id));
    let line = remedy_line(class, provider)?;
    if error.contains(&line) {
        return None;
    }
    let original = if error.trim().is_empty() {
        tail_chars(log_tail.trim(), 1_000)
    } else {
        error.trim_end().to_owned()
    };
    Some(if original.is_empty() {
        line
    } else {
        format!("{original}\n{line}")
    })
}

/// The last `max` characters of `text`.
fn tail_chars(text: &str, max: usize) -> String {
    let count = text.chars().count();
    text.chars().skip(count.saturating_sub(max)).collect()
}

#[cfg(test)]
mod tests {
    use super::CredentialFailure::{Missing, None as NoFailure, Rejected};
    use super::*;

    #[test]
    fn missing_credentials() {
        assert_eq!(classify("No API key found for zeta.", None), Missing);
        assert_eq!(classify("No API key for provider: zeta", None), Missing);
        assert_eq!(
            classify("No API key for zeta/zeta-large", Some("zeta-large")),
            Missing
        );
        assert_eq!(classify("no accounts: run /login zeta-cli", None), Missing);
        assert_eq!(classify("zeta: not logged in", None), Missing);
        assert_eq!(classify("zeta access token is missing", None), Missing);
    }

    #[test]
    fn authentication_required_only_without_a_model() {
        assert_eq!(
            classify("zeta authentication is required", Some("m1")),
            Missing
        );
        assert_eq!(
            classify(
                "authentication is required for zeta-large",
                Some("zeta-large")
            ),
            NoFailure
        );
        assert_eq!(
            classify("authentication is required to use this model", Some("m1")),
            NoFailure
        );
    }

    #[test]
    fn rejected_credentials() {
        assert_eq!(classify("HTTP 401 from zeta", None), Rejected);
        assert_eq!(classify("OAuth refresh failed for zeta", None), Rejected);
        assert_eq!(
            classify(
                "Authentication failed for \"zeta\". Credentials may have expired or network is unavailable. Run '/login zeta' to re-authenticate.",
                None
            ),
            Rejected
        );
        assert_eq!(
            classify(
                "Meta session expired (status 401). Run `/login meta` to sign in again.",
                None
            ),
            Rejected
        );
        assert_eq!(classify("error: invalid_grant", None), Rejected);
        assert_eq!(classify("4010 items", None), NoFailure);
    }

    #[test]
    fn forbidden_only_about_the_account() {
        assert_eq!(
            classify("403: the key of this account was disabled", None),
            Rejected
        );
        assert_eq!(
            classify("403: plan does not include zeta-large", Some("zeta-large")),
            NoFailure
        );
        assert_eq!(classify("403 access denied", None), NoFailure);
        // Account words count on word boundaries only.
        assert_eq!(
            classify("403: keyword filter blocked the request", None),
            NoFailure
        );
        assert_eq!(classify("forbidden: monkey business", None), NoFailure);
        assert_eq!(classify("403 forbidden: invalid token", None), Rejected);
        assert_eq!(classify("403: api_key disabled", None), Rejected);
        assert_eq!(classify("403 (key revoked)", None), Rejected);
        // Plurals count too.
        assert_eq!(
            classify("403 forbidden: invalid credentials", None),
            Rejected
        );
        assert_eq!(classify("403 forbidden: tokens revoked", None), Rejected);
        assert_eq!(classify("403: accounts are suspended", None), Rejected);
        assert_eq!(classify("forbidden: keys disabled", None), Rejected);
    }

    #[test]
    fn expiry_needs_the_engine_texts() {
        assert_eq!(
            classify("cache session expired, reconnecting", None),
            NoFailure
        );
        assert_eq!(classify("upstream session has expired", None), NoFailure);
        assert_eq!(
            classify("Your session has expired. Run /login meta.", None),
            Rejected
        );
        assert_eq!(
            classify("refresh: token expired, retrying", None),
            NoFailure
        );
        assert_eq!(classify("csrf token has expired", None), NoFailure);
        assert_eq!(classify("OAuth token was rejected by zeta", None), Rejected);
    }

    // task.md Delta D2 test 10 (matching rules clarified 2026-11-01: whole words, plurals).
    #[test]
    fn d2_test_10_account_words_are_whole_words() {
        assert_eq!(
            classify("403 forbidden: invalid credentials", None),
            Rejected
        );
        assert_eq!(classify("403: tokens revoked", None), Rejected);
        assert_eq!(classify("403: invalid keyword in request", None), NoFailure);
    }

    // task.md Delta D2 test 11 (expiry counts only with `/login` in the next sentence).
    #[test]
    fn d2_test_11_expiry_needs_login_in_the_next_sentence() {
        assert_eq!(classify("session expired", None), NoFailure);
        assert_eq!(classify("token expired", None), NoFailure);
        assert_eq!(
            classify(
                "Session expired. Please contact support. Run /login zeta",
                None
            ),
            NoFailure
        );
        assert_eq!(classify("Session expired. Run /login zeta", None), Rejected);
        assert_eq!(
            classify("session expired\nRun /login zeta to sign in again.", None),
            Rejected
        );
        assert_eq!(
            classify("Run /login zeta. Later: session expired.", None),
            NoFailure
        );
    }

    // task.md Delta D2: "the word model/models" is a whole word as well.
    #[test]
    fn model_is_matched_as_a_word() {
        assert_eq!(
            classify("zeta authentication is required for remodeling", Some("m1")),
            Missing
        );
        assert_eq!(
            classify("zeta authentication is required for all models", Some("m1")),
            NoFailure
        );
    }

    #[test]
    fn unrelated_failures() {
        assert_eq!(
            classify(
                "No model selected.\n\nUse /login to sign in to a provider, or set a provider's API key in the environment.",
                None
            ),
            NoFailure
        );
        assert_eq!(
            classify("daily limit reached, try again tomorrow", None),
            NoFailure
        );
        assert_eq!(classify("500 internal error", None), NoFailure);
    }

    #[test]
    fn rejected_wins_over_missing() {
        assert_eq!(
            classify("401 unauthorized; no accounts: run /login zeta", None),
            Rejected
        );
    }

    #[test]
    fn remedy_lines() {
        let error = annotate(None, "No API key found for zeta.\n", Some("zeta/m1")).expect("line");
        assert!(error.starts_with("No API key found for zeta."), "{error}");
        for needle in ["zeta", "missing", "/login zeta", "omo.jsonc"] {
            assert!(error.contains(needle), "{needle}: {error}");
        }
        let error = annotate(Some("HTTP 401 from acme"), "", Some("acme/m2")).expect("line");
        assert!(
            error.contains("rejected") && error.contains("/login acme"),
            "{error}"
        );
        assert_eq!(annotate(Some("401"), "", Some("m3")), None);
        assert_eq!(annotate(Some("exit 1"), "", Some("a/b")), None);
        // Added once.
        assert_eq!(annotate(Some(&error), "", Some("acme/m2")), None);
    }
}
