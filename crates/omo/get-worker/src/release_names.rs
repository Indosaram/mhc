//! Port of `src/release-names.ts`: channel names, asset allowlists, object keys.

pub const GITHUB_REPOSITORY: &str = "code-yeongyu/oh-my-openagent";

pub const CHANNELS: [&str; 2] = ["latest", "beta"];

pub type Channel = &'static str;

/// `X.Y.Z` with an optional dotted pre-release (same shape as the upstream `VERSION`).
#[must_use]
pub fn is_release_version(value: &str) -> bool {
    let mut segments = value.splitn(2, '-');
    let core = segments.next().unwrap_or("");
    let pre = segments.next();
    let mut numbers = core.split('.');
    let (Some(major), Some(minor), Some(patch), None) =
        (numbers.next(), numbers.next(), numbers.next(), numbers.next())
    else {
        return false;
    };
    if !major.chars().all(|c| c.is_ascii_digit())
        || !minor.chars().all(|c| c.is_ascii_digit())
        || !patch.chars().all(|c| c.is_ascii_digit())
    {
        return false;
    }
    if major.is_empty() || minor.is_empty() || patch.is_empty() {
        return false;
    }
    match pre {
        None => true,
        Some(pre) => {
            !pre.is_empty()
                && pre.split('.').all(|part| {
                    !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric())
                })
        }
    }
}

#[must_use]
pub fn is_channel(value: &str) -> bool {
    CHANNELS.iter().any(|channel| *channel == value)
}

/// Resolves a channel name to its static identity, mirroring `isChannel`'s narrowing.
#[must_use]
pub fn channel_of(value: &str) -> Option<Channel> {
    CHANNELS.iter().copied().find(|channel| *channel == value)
}

#[must_use]
pub fn is_beta_version(version: &str) -> bool {
    version.contains('-')
}

/// The asset kinds a release attaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AssetKind {
    Binary,
    Checksums,
    Engine,
}

/// Classifies a release asset name; `None` is upstream's `null` (rejected by `serve-release`).
#[must_use]
pub fn asset_kind(name: &str) -> Option<AssetKind> {
    if name == "SHA256SUMS" {
        return Some(AssetKind::Checksums);
    }
    if is_binary_asset(name) {
        return Some(AssetKind::Binary);
    }
    if is_engine_asset(name) {
        return Some(AssetKind::Engine);
    }
    None
}

fn is_binary_asset(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("omo-") else {
        return false;
    };
    let rest = rest.strip_suffix(".exe").unwrap_or(rest);
    let mut parts = rest.split('-');
    let Some(os) = parts.next() else { return false };
    let Some(arch) = parts.next() else { return false };
    if !matches!(os, "darwin" | "linux" | "windows") || !matches!(arch, "x64" | "arm64") {
        return false;
    }
    for part in parts {
        if !matches!(part, "musl" | "baseline") {
            return false;
        }
    }
    true
}

fn is_engine_asset(name: &str) -> bool {
    let Some(rest) = name.strip_prefix("senpi-desktop-engine-") else {
        return false;
    };
    if rest == "checksums.txt" {
        return true;
    }
    let rest = rest.strip_suffix(".exe").unwrap_or(rest);
    let Some((os, arch)) = rest.rsplit_once('-') else {
        return false;
    };
    matches!(os, "darwin" | "linux" | "win32") && matches!(arch, "x64" | "arm64")
}

#[must_use]
pub fn release_object_key(version: &str, asset: &str) -> String {
    format!("releases/v{version}/{asset}")
}

#[must_use]
pub fn completion_marker_key(version: &str) -> String {
    format!("releases/v{version}/.complete")
}

#[must_use]
pub fn channel_pointer_key(channel: Channel) -> String {
    format!("channels/{channel}")
}

#[must_use]
pub fn github_asset_url(version: &str, asset: &str) -> String {
    format!("https://github.com/{GITHUB_REPOSITORY}/releases/download/v{version}/{asset}")
}

/// npm publishes pre-releases as `X.Y.Z-0.beta.N`; the GitHub tag drops the `0.`.
#[must_use]
pub fn release_version_of_npm_version(npm_version: &str) -> String {
    npm_version.replacen("-0.", "-", 1)
}
