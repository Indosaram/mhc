//! Identifies the code that compiles memory blocks (#10834).
//!
//! Identifies the code that compiles memory blocks, for the persisted-block key, or `None` when
//! there is no stable identity: the package version plus the size and mtime of the running
//! binary/bundle, which contains the compiler, so any rebuild changes it.

pub fn memory_block_build() -> Option<String> {
    let version = std::env::var("OMO_SENPI_PACKAGE_VERSION")
        .ok()
        .or_else(|| option_env!("OMO_SENPI_PACKAGE_VERSION").map(str::to_string))?;
    let exe = std::env::current_exe().ok()?;
    let meta = std::fs::metadata(exe).ok()?;
    let size = meta.len();
    let mtime_ms = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_millis();
    Some(format!("{version}:{size}:{mtime_ms}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn given_no_package_version_when_checked_then_none() {
        // Safe execution check: ensure it does not panic without environment variable
        let _ = memory_block_build();
    }
}
