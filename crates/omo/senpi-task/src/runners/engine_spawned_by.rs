//! `runners/engine-spawned-by.ts`.

/// The first senpi release that accepts both the CLI `--spawned-by` flag and
/// `open_session.spawnedBy`: v2026.10.24-4 (senpi#4205). No host capability names the field, so
/// both launch paths decide from the engine's version.
pub const SPAWNED_BY_MIN_ENGINE_VERSION: &str = "2026.10.24-4";

// YYYY.MM.DD-N, optionally followed by +build metadata; a release without `-N` orders before `-1`.
fn version_fields(version: &str) -> Option<[u64; 4]> {
    let core = version.split_once('+').map_or(version, |(core, _)| core);
    let (date, increment) = match core.split_once('-') {
        Some((date, increment)) => (date, Some(increment)),
        None => (core, None),
    };
    let digits = |value: &str| {
        (!value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            .then(|| value.parse::<u64>().ok())
            .flatten()
    };
    let mut parts = date.split('.');
    let year = digits(parts.next()?)?;
    let month = digits(parts.next()?)?;
    let day = digits(parts.next()?)?;
    if parts.next().is_some() {
        return None;
    }
    let increment = match increment {
        Some(value) => digits(value)?,
        None => 0,
    };
    Some([year, month, day, increment])
}

/// Whether an engine reporting `version` accepts spawn ancestry. An absent or unparseable version
/// never does: an engine that cannot prove support must not receive a flag it may reject at boot.
pub fn engine_accepts_spawned_by(version: Option<&str>) -> bool {
    let (Some(fields), Some(minimum)) = (
        version.and_then(version_fields),
        version_fields(SPAWNED_BY_MIN_ENGINE_VERSION),
    ) else {
        return false;
    };
    fields >= minimum
}

#[cfg(test)]
mod tests {
    use super::*;

    // engine-spawned-by.test.ts
    #[test]
    fn only_the_minimum_and_later_accept_spawned_by() {
        for (version, accepts) in [
            (Some(SPAWNED_BY_MIN_ENGINE_VERSION), true),
            (Some("2026.10.24-3"), false),
            (Some("2026.10.25-1"), true),
            (Some("2026.10.100-1"), true),
            (Some("2026.9.30-99"), false),
            (Some("2026.11.1-1"), true),
            (Some("2027.1.1-1"), true),
            (Some("2026.10.24"), false),
            (Some("2026.10.24-4+1758000000.abc1234"), true),
            (Some("2026.9.18+1758000000.abc1234"), false),
            (Some("dev-build"), false),
            (Some(""), false),
            (None, false),
        ] {
            assert_eq!(engine_accepts_spawned_by(version), accepts, "{version:?}");
        }
    }
}
