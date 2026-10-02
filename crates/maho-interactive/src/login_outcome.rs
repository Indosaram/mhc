//! Port of interactive/login-outcome.ts.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginMethod { Oauth, ApiKey }

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoginFailure {
    Cancelled,
    Abort { message: String },
    CredentialSynchronization { message: String },
    Failed { message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoticeLevel { Status, Error }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginFailureNotice {
    pub level: NoticeLevel,
    pub message: String,
}

pub const LOGIN_CANCELLED_NOTICE: &str = "Login cancelled";

pub fn is_login_cancellation(error: Option<&LoginFailure>) -> bool {
    match error {
        None | Some(LoginFailure::Cancelled | LoginFailure::Abort { .. }) => true,
        Some(LoginFailure::CredentialSynchronization { message } | LoginFailure::Failed { message }) => {
            message == LOGIN_CANCELLED_NOTICE
        }
    }
}

pub fn describe_login_failure(error: Option<&LoginFailure>, provider: &str, method: LoginMethod) -> LoginFailureNotice {
    if is_login_cancellation(error) {
        return LoginFailureNotice { level: NoticeLevel::Status, message: LOGIN_CANCELLED_NOTICE.into() };
    }
    let message = match error {
        Some(LoginFailure::CredentialSynchronization { message }) => {
            let done = match method {
                LoginMethod::Oauth => format!("Logged in to {provider}"),
                LoginMethod::ApiKey => format!("Saved API key for {provider}"),
            };
            format!("{done}, but local model state could not be synchronized: {message}")
        }
        Some(LoginFailure::Failed { message }) => {
            let failed = match method {
                LoginMethod::Oauth => format!("Failed to login to {provider}"),
                LoginMethod::ApiKey => format!("Failed to save API key for {provider}"),
            };
            format!("{failed}: {message}")
        }
        None | Some(LoginFailure::Cancelled | LoginFailure::Abort { .. }) => LOGIN_CANCELLED_NOTICE.into(),
    };
    LoginFailureNotice { level: NoticeLevel::Error, message }
}
