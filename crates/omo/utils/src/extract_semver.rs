//! Semver extraction from noisy CLI output.

use std::sync::LazyLock;

use fancy_regex::Regex;

// The lookbehind rejects the milliseconds segment of timestamps like `00:24:25.202`.
static SEMVER_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?<![\d:])v?(\d+\.\d+\.\d+(?:[-+][\w.]+)*)")
        .unwrap_or_else(|error| panic!("{error}"))
});

pub fn extract_semver_from_output(output: &str) -> Option<String> {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return None;
    }
    let captures = SEMVER_PATTERN.captures(trimmed).ok().flatten()?;
    captures.get(1).map(|m| m.as_str().to_string())
}
