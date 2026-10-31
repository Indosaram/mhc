//! Which access token `/feedback` and `/bug` send (delta D3): the stored CLI sign-in for the
//! service API origin when it is usable as stored, else an anonymous send with one warning.

use jiff::{SignedDuration, Timestamp};

/// A stored token must stay valid for longer than this to be sent.
pub const MIN_TOKEN_VALIDITY: SignedDuration = SignedDuration::from_secs(30);

/// Warning shown before an anonymous send when a stored sign-in could not be used.
pub const UNUSABLE_SIGN_IN_WARNING: &str = "Your mhc sign-in could not be used (it expired or its state is unclear), so this report will be sent without your account. Sign in again to link future reports.";

/// A stored CLI sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredSignIn {
    pub access_token: String,
    pub expires_at: Timestamp,
    /// The last refresh outcome is unknown (for example interrupted mid-flight).
    pub refresh_uncertain: bool,
}

/// Why the credential store returned no sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SignInError {
    /// The user is signed out; not a problem.
    SignedOut,
    /// The store could not be read.
    Failed(String),
}

/// The credential store of the service API origin.
pub trait SignInStore: Send + Sync {
    /// The stored sign-in; `Ok(None)` when nothing is stored.
    fn read(&self) -> Result<Option<StoredSignIn>, SignInError>;
    /// Refreshes the stored sign-in. The feedback flow never calls this: a token that is not
    /// usable as stored is treated as unusable.
    fn refresh(&self) -> Result<Option<StoredSignIn>, SignInError>;
}

/// The token for a submit and the warning to show before sending.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Access {
    pub token: Option<String>,
    pub warning: Option<String>,
}

/// Decides the access token for a submit. `caller_token` is only used when no store is
/// configured (the pre-existing behaviour).
pub fn decide_access(
    uploads_disabled: bool,
    store: Option<&dyn SignInStore>,
    now: Timestamp,
    caller_token: Option<&str>,
) -> Access {
    if uploads_disabled {
        return Access::default();
    }
    let Some(store) = store else {
        return Access {
            token: caller_token.map(str::to_owned),
            warning: None,
        };
    };
    let unusable = Access {
        token: None,
        warning: Some(UNUSABLE_SIGN_IN_WARNING.to_owned()),
    };
    match store.read() {
        Ok(None) | Err(SignInError::SignedOut) => Access::default(),
        Err(SignInError::Failed(_)) => unusable,
        Ok(Some(sign_in)) => {
            let usable = !sign_in.refresh_uncertain
                && sign_in.expires_at.duration_since(now) > MIN_TOKEN_VALIDITY;
            if usable {
                Access {
                    token: Some(sign_in.access_token),
                    warning: None,
                }
            } else {
                unusable
            }
        }
    }
}
