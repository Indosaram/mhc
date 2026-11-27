//! Port of senpi packages/coding-agent/src/core/engine-build-identity.ts.
//!
//! Build identity as an ORDINAL rather than a version string. Two hosts of the same protocol decide
//! which is newer by comparing the ordinal, never the version strings (CalVer -N is a post-release
//! increment, the opposite of a semver reading).

use std::sync::OnceLock;

/// The engine's own CalVer-style version. maho keeps senpi's version string shape: a release build's
/// `MHC_BUILD_VERSION` (what `mhc --version` prints), else the crate version.
pub const ENGINE_VERSION: &str = match option_env!("SENPI_ENGINE_VERSION") {
    Some(version) if !version.is_empty() => version,
    _ => match option_env!("MHC_BUILD_VERSION") {
        Some(version) => version,
        None => env!("CARGO_PKG_VERSION"),
    },
};

/// No senpi release predates 2000, so a CalVer ordinal's first part is a year at or after it.
pub const FIRST_CALVER_YEAR: i64 = 2000;

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
        let take = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
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
                    let take = after
                        .find(|c: char| !c.is_ascii_digit())
                        .unwrap_or(after.len());
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
        scheme: if epoch == 0 {
            EngineOrdinalScheme::Nodef
        } else {
            EngineOrdinalScheme::Epoch
        },
    }
}

fn built() -> &'static EngineBuildIdentity {
    static BUILT: OnceLock<EngineBuildIdentity> = OnceLock::new();
    BUILT.get_or_init(|| {
        let epoch = std::env::var("MAHO_BUILD_EPOCH")
            .ok()
            .and_then(|value| value.parse::<i64>().ok());
        let sha7 = std::env::var("MAHO_BUILD_SHA7")
            .ok()
            .filter(|value| !value.is_empty());
        engine_build_identity_from(&EngineBuildInput {
            version: ENGINE_VERSION.to_owned(),
            epoch,
            sha7,
        })
    })
}

/// Identity of the running engine build. Constant for the life of the process.
pub fn engine_build_identity() -> &'static EngineBuildIdentity {
    built()
}

/// Whether both ordinals are senpi CalVer, the only vocabulary two builds are ordered in (senpi#7040).
pub fn comparable_engines(a: &EngineBuildIdentity, b: &EngineBuildIdentity) -> bool {
    a.ordinal[0] >= FIRST_CALVER_YEAR && b.ordinal[0] >= FIRST_CALVER_YEAR
}

/// Orders two builds: -1 older, 1 newer, 0 equal OR uncomparable. The build epoch breaks a tie ONLY
/// when BOTH sides carry one, so a build of unknown age can never outrank a known one.
pub fn compare_engine_ordinal(a: &EngineBuildIdentity, b: &EngineBuildIdentity) -> i32 {
    if !comparable_engines(a, b) {
        return 0;
    }
    for index in 0..4 {
        if a.ordinal[index] != b.ordinal[index] {
            return if a.ordinal[index] > b.ordinal[index] {
                1
            } else {
                -1
            };
        }
    }
    if a.scheme != EngineOrdinalScheme::Epoch
        || b.scheme != EngineOrdinalScheme::Epoch
        || a.ordinal[4] == b.ordinal[4]
    {
        return 0;
    }
    if a.ordinal[4] > b.ordinal[4] { 1 } else { -1 }
}

/// The identity a running host advertises (`engineVersion`, `engineOrdinal`): a non-zero epoch is an `epoch` build.
pub fn engine_identity_of(text: &str, ordinal: EngineOrdinal) -> EngineBuildIdentity {
    EngineBuildIdentity {
        text: text.to_owned(),
        ordinal,
        scheme: if ordinal[4] > 0 {
            EngineOrdinalScheme::Epoch
        } else {
            EngineOrdinalScheme::Nodef
        },
    }
}

/// Why handing `running` over to `successor` would DOWNGRADE it (senpi#7018), or `None` when it would not.
pub fn older_engine_detail(
    successor: &EngineBuildIdentity,
    running: &EngineBuildIdentity,
) -> Option<String> {
    if !comparable_engines(successor, running) {
        return Some(format!(
            "the successor's engine {} [{},{},{},{},{}] cannot be ordered against the running host's {} [{},{},{},{},{}]: only two senpi CalVer engine ordinals are comparable",
            successor.text,
            successor.ordinal[0],
            successor.ordinal[1],
            successor.ordinal[2],
            successor.ordinal[3],
            successor.ordinal[4],
            running.text,
            running.ordinal[0],
            running.ordinal[1],
            running.ordinal[2],
            running.ordinal[3],
            running.ordinal[4],
        ));
    }
    (compare_engine_ordinal(successor, running) < 0).then(|| {
        format!(
            "the successor's engine {} is older than the running host's {}",
            successor.text, running.text
        )
    })
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
        assert_eq!(
            identity("2026.9.16-3", None, None).ordinal,
            [2026, 9, 16, 3, 0]
        );
    }

    #[test]
    fn an_epoch_and_sha7_decorate_the_text() {
        let built = identity("2026.9.16", Some(1000), Some("abc1234"));
        assert_eq!(built.text, "2026.9.16+1000.abc1234");
        assert_eq!(built.scheme, EngineOrdinalScheme::Epoch);
    }

    #[test]
    fn a_zero_or_absent_epoch_selects_nodef() {
        assert_eq!(
            identity("2026.9.16", Some(0), Some("abc")).scheme,
            EngineOrdinalScheme::Nodef
        );
        assert_eq!(
            identity("2026.9.16", Some(0), Some("abc")).text,
            "2026.9.16"
        );
    }

    #[test]
    fn an_unparseable_version_is_ordinal_zero() {
        assert_eq!(
            identity("not-a-version", None, None).ordinal,
            [0, 0, 0, 0, 0]
        );
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

    #[test]
    fn non_calver_ordinals_are_uncomparable_and_refuse_handoff() {
        let calver = identity("2026.11.18", None, None);
        let semver = engine_identity_of("5.1.78", [5, 1, 78, 0, 0]);
        assert!(!comparable_engines(&calver, &semver));
        assert_eq!(compare_engine_ordinal(&calver, &semver), 0);
        assert!(older_engine_detail(&calver, &semver).is_some());
        let newer = identity("2026.11.19", None, None);
        assert_eq!(older_engine_detail(&newer, &calver), None);
        assert!(
            older_engine_detail(&calver, &newer)
                .is_some_and(|msg| msg.contains("is older than the running host's"))
        );
    }
}
