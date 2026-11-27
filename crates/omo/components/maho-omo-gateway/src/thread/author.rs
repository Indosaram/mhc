//! External author normalization (`author.ts`).

use super::errors::ThreadErrorCode;
use super::types::{ExternalAuthor, StoreRefusal};

pub const AUTHOR_ID_MAX_LENGTH: usize = 256;
pub const AUTHOR_DISPLAY_MAX_LENGTH: usize = 256;

fn refusal(message: String) -> StoreRefusal {
    StoreRefusal {
        code: ThreadErrorCode::parse("invalid_arguments")
            .unwrap_or_else(|| unreachable!("invalid_arguments is a valid code")),
        message,
        details: None,
    }
}

fn has_line_breaking_control(value: &str) -> bool {
    value
        .chars()
        .any(|c| c.is_control() || matches!(c, '\u{2028}' | '\u{2029}'))
}

fn validate_field(name: &str, value: &str, max: usize) -> Result<String, StoreRefusal> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(refusal(format!(
            "author.{name} must be a non-empty string."
        )));
    }
    if value.len() > max {
        return Err(refusal(format!(
            "author.{name} is longer than {max} characters."
        )));
    }
    if has_line_breaking_control(value) {
        return Err(refusal(format!(
            "author.{name} carries a control character or line break."
        )));
    }
    Ok(trimmed.to_owned())
}

/// Validates and trims `input`, returning `invalid_arguments` if any field is empty, over-length,
/// or carries control/line-breaking characters.
pub fn normalize_author(input: &ExternalAuthor) -> Result<ExternalAuthor, StoreRefusal> {
    let platform_user_id = validate_field(
        "platform_user_id",
        &input.platform_user_id,
        AUTHOR_ID_MAX_LENGTH,
    )?;
    let display = validate_field("display", &input.display, AUTHOR_DISPLAY_MAX_LENGTH)?;
    let user_id = match input.user_id.as_deref() {
        Some(raw) => Some(validate_field("user_id", raw, AUTHOR_ID_MAX_LENGTH)?),
        None => None,
    };
    Ok(ExternalAuthor {
        platform_user_id,
        display,
        user_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_valid_author_and_rejects_control_chars() {
        let ok = normalize_author(&ExternalAuthor {
            platform_user_id: " u-1 ".into(),
            display: " Alice ".into(),
            user_id: Some(" acct-1 ".into()),
        })
        .expect("valid");
        assert_eq!(ok.platform_user_id, "u-1");
        assert_eq!(ok.display, "Alice");
        assert_eq!(ok.user_id.as_deref(), Some("acct-1"));

        let err = normalize_author(&ExternalAuthor {
            platform_user_id: "u-1\nfake".into(),
            display: "Alice".into(),
            user_id: None,
        })
        .expect_err("control char rejected");
        assert_eq!(err.code.as_str(), "invalid_arguments");
    }
}
