/// POSIX single-quote a value for a shell command line.
#[must_use]
pub fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
