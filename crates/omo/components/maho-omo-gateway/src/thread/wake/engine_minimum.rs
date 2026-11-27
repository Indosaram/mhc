//! Minimum engine version gate for cold wake (`thread/wake/engine-minimum.ts`).

use std::sync::LazyLock;

use regex::Regex;

pub const MIN_WAKE_ENGINE_VERSION: Option<&str> = Some("2026.11.15-2");
const MIN_WAKE_OMO_VERSION: &str = "5.1.72";

static ENGINE_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d+)\.(\d+)\.(\d+)(?:-(\d+))?(?:[-+.].*)?$").expect("valid ENGINE_VERSION_RE")
});
static OMO_VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(\d+)\.(\d+)\.(\d+)(-[^+]*)?(?:\+.*)?$").expect("valid OMO_VERSION_RE")
});

fn fields_of(version: &str) -> Option<[i64; 4]> {
    let caps = ENGINE_VERSION_RE.captures(version)?;
    let y = caps.get(1)?.as_str().parse::<i64>().ok()?;
    let m = caps.get(2)?.as_str().parse::<i64>().ok()?;
    let d = caps.get(3)?.as_str().parse::<i64>().ok()?;
    let n = caps
        .get(4)
        .and_then(|g| g.as_str().parse::<i64>().ok())
        .unwrap_or(0);
    Some([y, m, d, n])
}

fn omo_release_fields(version: &str) -> Option<[i64; 4]> {
    let caps = OMO_VERSION_RE.captures(version)?;
    let major = caps.get(1)?.as_str().parse::<i64>().ok()?;
    if major >= 1000 {
        return None;
    }
    let minor = caps.get(2)?.as_str().parse::<i64>().ok()?;
    let patch = caps.get(3)?.as_str().parse::<i64>().ok()?;
    let pre = if caps.get(4).is_some() { -1 } else { 0 };
    Some([major, minor, patch, pre])
}

fn at_least(fields: [i64; 4], floor: [i64; 4]) -> bool {
    fields >= floor
}

/// Whether an engine reporting `version` drops the retained record on release.
pub fn engine_drops_retained_record(version: Option<&str>, minimum: Option<&str>) -> bool {
    let (Some(min_ver), Some(ver)) = (minimum, version) else {
        return false;
    };
    let is_omob = ver.starts_with("0.0.0-omob.");
    let omo = if is_omob {
        None
    } else {
        omo_release_fields(ver)
    };
    if is_omob || omo.is_some() {
        if Some(min_ver) != MIN_WAKE_ENGINE_VERSION {
            return false;
        }
        return is_omob
            || omo
                .zip(omo_release_fields(MIN_WAKE_OMO_VERSION))
                .is_some_and(|(a, b)| at_least(a, b));
    }
    match (fields_of(ver), fields_of(min_ver)) {
        (Some(f), Some(fl)) => at_least(f, fl),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_calver_and_omo_floors() {
        assert!(engine_drops_retained_record(
            Some("2026.11.15-2"),
            MIN_WAKE_ENGINE_VERSION
        ));
        assert!(engine_drops_retained_record(
            Some("2026.11.19-5"),
            MIN_WAKE_ENGINE_VERSION
        ));
        assert!(!engine_drops_retained_record(
            Some("2026.11.15"),
            MIN_WAKE_ENGINE_VERSION
        ));
        assert!(engine_drops_retained_record(
            Some("5.1.72"),
            MIN_WAKE_ENGINE_VERSION
        ));
        assert!(!engine_drops_retained_record(
            Some("5.1.72-beta.1"),
            MIN_WAKE_ENGINE_VERSION
        ));
        assert!(engine_drops_retained_record(
            Some("0.0.0-omob.abc.def"),
            MIN_WAKE_ENGINE_VERSION
        ));
    }
}
