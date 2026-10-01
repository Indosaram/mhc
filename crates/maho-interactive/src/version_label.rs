//! Port of version-label.ts.
pub fn format_display_version(version: &str) -> String {
    if version.as_bytes().first().is_some_and(u8::is_ascii_digit) {
        format!("v{version}")
    } else {
        version.into()
    }
}
