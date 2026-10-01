//! Port of senpi packages/coding-agent/src/core/engine-build-identity.ts.
//!
//! Build identity as an ORDINAL rather than a version string. Two hosts of the same protocol decide
//! which is newer by comparing the ordinal, never the version strings (CalVer -N is a post-release
//! increment, the opposite of a semver reading).

use std::sync::OnceLock;

/// The engine's own CalVer-style version. maho keeps senpi's version string shape.
pub const ENGINE_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EngineOrdinalScheme {
    Epoch,
    Nodef,
}

/// [year, month, day, postReleaseIncrement, buildEpochSeconds].
pub type EngineOrdinal = [i64; 5];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineBuildIdentity {
    pub text: String,
    pub ordinal: EngineOrdinal,
    pub scheme: EngineOrdinalScheme,
}

#[derive(Debug, Clone, Default)]
pub struct EngineBuildInput {
    pub version: String,
    pub epoch: Option<i64>,
    pub sha7: Option<String>,
}

fn parse_calver(version: &str) -> Option<(i64, i64, i64, i64)> {
    let mut rest = version;
    let mut parts = [0i64; 4];
    for index in 0..4 {
        let take = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
        if take == 0 {
            return None;
        }
        parts[index] = rest[..take].parse().ok()?;
        rest = &rest[take..];
        match index {
            0 | 1 => {
                rest = rest.strip_prefix('.')?;
            }
            _ => {
                if let Some(after) = rest.strip_prefix('-') {
                    let take = after.find(|c: char| !c.is_ascii_digit()).unwrap_or(after.len());
                    if take == 0 {
                        return None;
                    }
                    parts[3] = after[..take].parse().ok()?;
                }
                break;
            }
        }
    }
    Some((parts[0], parts[1], parts[2], parts[3]))
}

/// Builds an identity from explicit inputs. Never fails: an unparseable version is ordinal zero.
pub fn engine_build_identity_from(build: &EngineBuildInput) -> EngineBuildIdentity {
    let parsed = parse_calver(&build.version);
    let epoch = match build.epoch {
        Some(epoch) if epoch > 0 => epoch,
        _ => 0,
    };
    let sha7 = build.sha7.clone().unwrap_or_default();
    let text = if epoch == 0 {
        build.version.clone()
    } else if sha7.is_empty() {
        format!("{}+{epoch}", build.version)
    } else {
        format!("{}+{epoch}.{sha7}", build.version)
    };
    let (year, month, day, increment) = parsed.unwrap_or((0, 0, 0, 0));
    EngineBuildIdentity {
        text,
        ordinal: [year, month, day, increment, epoch],
        scheme: if epoch == 0 { EngineOrdinalScheme::Nodef } else { EngineOrdinalScheme::Epoch },
    }
}

fn built() -> &'static EngineBuildIdentity {
    static BUILT: OnceLock<EngineBuildIdentity> = OnceLock::new();
    BUILT.get_or_init(|| {
        let epoch = std::env::var("MAHO_BUILD_EPOCH").ok().and_then(|value| value.parse::<i64>().ok());
        let sha7 = std::env::var("MAHO_BUILD_SHA7").ok().filter(|value| !value.is_empty());
        engine_build_identity_from(&EngineBuildInput { version: ENGINE_VERSION.to_owned(), epoch, sha7 })
    })
}

/// Identity of the running engine build. Constant for the life of the process.
pub fn engine_build_identity() -> &'static EngineBuildIdentity {
    built()
}

/// Orders two builds: -1 older, 1 newer, 0 equal OR uncomparable. The build epoch breaks a tie ONLY
/// when BOTH sides carry one, so a build of unknown age can never outrank a known one.
pub fn compare_engine_ordinal(a: &EngineBuildIdentity, b: &EngineBuildIdentity) -> i32 {
    for index in 0..4 {
        if a.ordinal[index] != b.ordinal[index] {
            return if a.ordinal[index] > b.ordinal[index] { 1 } else { -1 };
        }
    }
    if a.scheme != EngineOrdinalScheme::Epoch || b.scheme != EngineOrdinalScheme::Epoch || a.ordinal[4] == b.ordinal[4] {
        return 0;
    }
    if a.ordinal[4] > b.ordinal[4] { 1 } else { -1 }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(version: &str, epoch: Option<i64>, sha7: Option<&str>) -> EngineBuildIdentity {
        engine_build_identity_from(&EngineBuildInput {
            version: version.to_owned(),
            epoch,
            sha7: sha7.map(str::to_owned),
        })
    }

    #[test]
    fn parses_a_plain_calver() {
        let built = identity("2026.9.16", None, None);
        assert_eq!(built.ordinal, [2026, 9, 16, 0, 0]);
        assert_eq!(built.scheme, EngineOrdinalScheme::Nodef);
        assert_eq!(built.text, "2026.9.16");
    }

    #[test]
    fn parses_a_post_release_increment() {
        assert_eq!(identity("2026.9.16-3", None, None).ordinal, [2026, 9, 16, 3, 0]);
    }

    #[test]
    fn an_epoch_and_sha7_decorate_the_text() {
        let built = identity("2026.9.16", Some(1000), Some("abc1234"));
        assert_eq!(built.text, "2026.9.16+1000.abc1234");
        assert_eq!(built.scheme, EngineOrdinalScheme::Epoch);
    }

    #[test]
    fn a_zero_or_absent_epoch_selects_nodef() {
        assert_eq!(identity("2026.9.16", Some(0), Some("abc")).scheme, EngineOrdinalScheme::Nodef);
        assert_eq!(identity("2026.9.16", Some(0), Some("abc")).text, "2026.9.16");
    }

    #[test]
    fn an_unparseable_version_is_ordinal_zero() {
        assert_eq!(identity("not-a-version", None, None).ordinal, [0, 0, 0, 0, 0]);
    }

    #[test]
    fn post_release_sorts_after_the_base_release() {
        let base = identity("2026.9.16", None, None);
        let post = identity("2026.9.16-3", None, None);
        assert_eq!(compare_engine_ordinal(&base, &post), -1);
        assert_eq!(compare_engine_ordinal(&post, &base), 1);
    }

    #[test]
    fn an_epoch_breaks_a_tie_only_when_both_are_known() {
        let older = identity("2026.9.16", Some(1000), None);
        let newer = identity("2026.9.16", Some(2000), None);
        assert_eq!(compare_engine_ordinal(&older, &newer), -1);
        let unknown = identity("2026.9.16", None, None);
        assert_eq!(compare_engine_ordinal(&older, &unknown), 0);
        assert_eq!(compare_engine_ordinal(&unknown, &newer), 0);
        assert_eq!(compare_engine_ordinal(&older, &older), 0);
    }
}
