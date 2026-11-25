use std::path::Path;

/// `"bun"` or `"node"` from an executable path; `None` for anything else (a compiled senpi binary,
/// for one).
pub fn runtime_name_of(command: &str) -> Option<&'static str> {
    // Strip both `/` and `\` suffixes so Windows-style paths resolve consistently on POSIX tests.
    let file_name = command
        .rsplit(['/', '\\'])
        .next()
        .filter(|segment| !segment.is_empty())
        .or_else(|| {
            Path::new(command)
                .file_name()
                .and_then(|name| name.to_str())
        })?;
    let lower = file_name.to_ascii_lowercase();
    let stem = lower
        .strip_suffix(".exe")
        .or_else(|| lower.strip_suffix(".cmd"))
        .or_else(|| lower.strip_suffix(".bat"))
        .unwrap_or(&lower);
    match stem {
        "bun" => Some("bun"),
        "node" => Some("node"),
        _ => None,
    }
}
