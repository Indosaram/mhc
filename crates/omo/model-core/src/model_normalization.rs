/// Trims a model string; blank or missing becomes `None`.
#[must_use]
pub fn normalize_model(model: Option<&str>) -> Option<String> {
    let trimmed = model?.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Rewrites `.<digits>` to `-<digits>` (`claude-3.5-sonnet` -> `claude-3-5-sonnet`).
#[must_use]
pub fn normalize_model_id(model_id: &str) -> String {
    let mut result = String::with_capacity(model_id.len());
    let mut chars = model_id.chars().peekable();
    while let Some(current) = chars.next() {
        if current == '.' && chars.peek().is_some_and(char::is_ascii_digit) {
            result.push('-');
        } else {
            result.push(current);
        }
    }
    result
}
