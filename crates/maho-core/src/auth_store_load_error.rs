//! Port of senpi packages/coding-agent/src/core/auth-store-load-error.ts (v2026.11.19-5): the parse half.
//!
//! The mutable store reads auth.json with `parseAuthDocument`, which refuses only a document that is
//! not a JSON object. Per-credential validation (`validateReadOnlyAuthData`) belongs to the read-only
//! store; applying it here would refuse the pooled shape a newer sign-in writes (an OAuth entry whose
//! tokens live only in `accounts[]`) and with it every other credential in the file.

use std::borrow::Cow;
use std::fmt;

use crate::auth_storage::AuthStorageData;
use crate::text::strip_bom;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStoreLoadErrorKind {
    MalformedJson,
    InvalidFormat,
    Unreadable,
    WriteBackFailed,
}

/// Why an existing auth.json could not be loaded (senpi#4413). `reason` never carries the file's path
/// or contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStoreLoadError {
    pub kind: AuthStoreLoadErrorKind,
    pub reason: Cow<'static, str>,
}

impl AuthStoreLoadError {
    /// senpi `AuthStoreUnreadableError` message.
    pub fn message(&self) -> String {
        format!(
            "Your sign-in store (auth.json) could not be read: {}. Nothing was changed; repair or move auth.json in the agent directory, then sign in again.",
            self.reason
        )
    }
}

/// A store load that failed after auth.json was read and parsed: saving its repaired or migrated copy
/// failed (`wrote_back`), or the lock was lost once the read was done. Neither means the file is unreadable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStoreAfterReadError {
    pub wrote_back: bool,
    pub detail: String,
    pub errno_code: Option<String>,
}

impl fmt::Display for AuthStoreAfterReadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.wrote_back {
            write!(
                f,
                "auth.json was read, but saving its repaired or migrated copy failed: {}",
                self.detail
            )
        } else {
            write!(
                f,
                "auth.json was read, but its lock was lost before the read finished: {}",
                self.detail
            )
        }
    }
}

impl std::error::Error for AuthStoreAfterReadError {}

/// The load error a failed store read stands for, or `None` when the failure says nothing about the
/// file: a lock held by another writer, a read the caller aborted, or a lock lost after a clean read.
pub fn describe_auth_store_load_failure(
    parsed_error: Option<&AuthStoreLoadError>,
    after_read: Option<&AuthStoreAfterReadError>,
    is_busy: bool,
    is_aborted: bool,
    errno_code: Option<&str>,
) -> Option<AuthStoreLoadError> {
    if let Some(load_error) = parsed_error {
        return Some(load_error.clone());
    }
    if is_busy || is_aborted {
        return None;
    }
    if let Some(after) = after_read {
        if !after.wrote_back {
            return None;
        }
        let base = "it was read, but its repaired or migrated copy could not be saved";
        let reason = match after.errno_code.as_deref() {
            Some(code) if !code.is_empty() => Cow::Owned(format!("{base} ({code})")),
            _ => Cow::Borrowed(base),
        };
        return Some(AuthStoreLoadError {
            kind: AuthStoreLoadErrorKind::WriteBackFailed,
            reason,
        });
    }
    let reason = match errno_code {
        Some(code) if !code.is_empty() => {
            Cow::Owned(format!("the file could not be read ({code})"))
        }
        _ => Cow::Borrowed("the file could not be read"),
    };
    Some(AuthStoreLoadError {
        kind: AuthStoreLoadErrorKind::Unreadable,
        reason,
    })
}

/// The top-level object of an auth.json document; anything else is refused rather than read as empty.
pub fn parse_auth_document(content: &str) -> Result<AuthStorageData, AuthStoreLoadError> {
    let parsed: serde_json::Value =
        serde_json::from_str(strip_bom(content)).map_err(|_| AuthStoreLoadError {
            kind: AuthStoreLoadErrorKind::MalformedJson,
            reason: Cow::Borrowed("it is not valid JSON"),
        })?;
    match parsed {
        serde_json::Value::Object(object) => Ok(object),
        _ => Err(AuthStoreLoadError {
            kind: AuthStoreLoadErrorKind::InvalidFormat,
            reason: Cow::Borrowed("it does not hold a JSON object"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_oauth_entry_with_tokens_only_in_accounts_parses() {
        let data = parse_auth_document(
            r#"{"chatgpt-subscription":{"type":"oauth","expires":1,"accountId":"a","accounts":[{"name":"login","source":"login","access":"x","refresh":"y","expires":2,"route":"siwc"}]},"inferhub":{"type":"api_key","key":"k"}}"#,
        )
        .expect("pooled document");
        assert_eq!(data.len(), 2);
        assert_eq!(
            data["chatgpt-subscription"]["accounts"][0]["route"],
            serde_json::json!("siwc")
        );
    }

    #[test]
    fn malformed_and_non_object_documents_are_refused() {
        assert_eq!(
            parse_auth_document("{oops").expect_err("malformed").kind,
            AuthStoreLoadErrorKind::MalformedJson
        );
        let error = parse_auth_document("[]").expect_err("array");
        assert_eq!(error.kind, AuthStoreLoadErrorKind::InvalidFormat);
        assert!(!error.message().contains("[]"));
    }
}
