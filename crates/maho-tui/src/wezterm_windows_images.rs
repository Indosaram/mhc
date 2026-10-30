//! Port of senpi `packages/tui/src/wezterm-windows-images.ts`.
//!
//! WezTerm on Windows runs its shell through the ConPTY it bundles. Bundled
//! ConPTY builds before 1.22 drop the Kitty graphics APC (`ESC _G ... ESC \`)
//! before it reaches WezTerm, so an image placement leaves a blank gap. WezTerm
//! bundled ConPTY 1.22.250204002 in wezterm/wezterm@4accc376f, whose version
//! string is below (committer date in its own zone, see wezterm-version/build.rs);
//! that build and later ones deliver the APC. No stable release has it yet.

use std::sync::LazyLock;

use regex::Regex;

pub const MIN_WEZTERM_WINDOWS_IMAGES_VERSION: &str = "20250208-160106";

pub const WEZTERM_WINDOWS_IMAGES_HINT: &str =
    "update WezTerm to a nightly build to show images inline";

// WezTerm sets TERM_PROGRAM_VERSION to `wezterm_version()`: YYYYMMDD-HHMMSS-<commit>.
static WEZTERM_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(\d{8})-(\d{6})-[0-9a-f]+").expect("valid regex"));

/// True only when WezTerm on Windows positively reports a build older than the
/// first one whose bundled ConPTY passes Kitty graphics through. A missing or
/// unparsable version keeps images on: there is no evidence of an old build.
/// `platform` uses Node's names (`"win32"`, `"linux"`, `"darwin"`).
pub fn is_old_windows_wezterm(version: Option<&str>, platform: &str) -> bool {
    let Some(version) = version else {
        return false;
    };
    if platform != "win32" {
        return false;
    }
    let Some(captures) = WEZTERM_VERSION.captures(version) else {
        return false;
    };
    format!("{}-{}", &captures[1], &captures[2]).as_str() < MIN_WEZTERM_WINDOWS_IMAGES_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;

    const OLD_STABLE: &str = "20240203-110809-5046fc22";
    const LAST_BAD: &str = "20250208-160105-0123abcd";
    const FIRST_GOOD: &str = "20250208-160106-4accc376";
    const NIGHTLY: &str = "20261010-084512-a1b2c3d4";

    #[test]
    fn old_builds_on_windows_are_detected() {
        assert!(is_old_windows_wezterm(Some(OLD_STABLE), "win32"));
        assert!(is_old_windows_wezterm(Some(LAST_BAD), "win32"));
    }

    #[test]
    fn good_missing_or_malformed_versions_keep_images() {
        for version in [
            Some(FIRST_GOOD),
            Some(NIGHTLY),
            None,
            Some("nightly"),
            Some(""),
        ] {
            assert!(!is_old_windows_wezterm(version, "win32"), "{version:?}");
        }
    }

    #[test]
    fn old_wezterm_outside_windows_keeps_images() {
        for platform in ["linux", "darwin"] {
            assert!(!is_old_windows_wezterm(Some(OLD_STABLE), platform));
        }
    }
}
