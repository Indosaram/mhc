//! Comment-checker release descriptors (port of `src/release.ts`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Release pin: <https://github.com/code-yeongyu/go-claude-code-comment-checker/releases>
pub const COMMENT_CHECKER_RELEASE_VERSION: &str = "0.8.0";
pub const COMMENT_CHECKER_RELEASE_REPO: &str = "code-yeongyu/go-claude-code-comment-checker";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommentCheckerArchiveExtension {
    TarGz,
    Zip,
}

impl CommentCheckerArchiveExtension {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::TarGz => "tar.gz",
            Self::Zip => "zip",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentCheckerReleaseAsset {
    pub os: String,
    pub arch: String,
    pub ext: CommentCheckerArchiveExtension,
    pub asset_name: String,
    pub url: String,
}

struct ReleasePlatform {
    os: &'static str,
    arch: &'static str,
    ext: CommentCheckerArchiveExtension,
}

fn release_platform(key: &str) -> Option<ReleasePlatform> {
    match key {
        "darwin-arm64" => Some(ReleasePlatform {
            os: "darwin",
            arch: "arm64",
            ext: CommentCheckerArchiveExtension::TarGz,
        }),
        "darwin-x64" => Some(ReleasePlatform {
            os: "darwin",
            arch: "amd64",
            ext: CommentCheckerArchiveExtension::TarGz,
        }),
        "linux-arm64" => Some(ReleasePlatform {
            os: "linux",
            arch: "arm64",
            ext: CommentCheckerArchiveExtension::TarGz,
        }),
        "linux-x64" => Some(ReleasePlatform {
            os: "linux",
            arch: "amd64",
            ext: CommentCheckerArchiveExtension::TarGz,
        }),
        "win32-x64" => Some(ReleasePlatform {
            os: "windows",
            arch: "amd64",
            ext: CommentCheckerArchiveExtension::Zip,
        }),
        _ => None,
    }
}

pub fn resolve_comment_checker_release_asset(
    platform: &str,
    arch: &str,
    version: Option<&str>,
) -> Option<CommentCheckerReleaseAsset> {
    let version = version.unwrap_or(COMMENT_CHECKER_RELEASE_VERSION);
    let key = format!("{platform}-{arch}");
    let target = release_platform(&key)?;
    let asset_name = format!(
        "comment-checker_v{version}_{}_{}.{}",
        target.os,
        target.arch,
        target.ext.as_str()
    );
    let url = format!(
        "https://github.com/{COMMENT_CHECKER_RELEASE_REPO}/releases/download/v{version}/{asset_name}"
    );
    Some(CommentCheckerReleaseAsset {
        os: target.os.to_string(),
        arch: target.arch.to_string(),
        ext: target.ext,
        asset_name,
        url,
    })
}

pub fn comment_checker_binary_name(platform: &str) -> &'static str {
    if platform == "win32" {
        "comment-checker.exe"
    } else {
        "comment-checker"
    }
}

pub struct CommentCheckerCacheDirInput<'a> {
    pub platform: &'a str,
    pub env: &'a BTreeMap<String, String>,
    pub homedir: &'a Path,
    pub cache_dir_name: &'a str,
}

/// `%LOCALAPPDATA%\<name>\bin` on Windows, `$XDG_CACHE_HOME/<name>/bin` (default `~/.cache`) elsewhere.
pub fn comment_checker_cache_dir(input: &CommentCheckerCacheDirInput<'_>) -> PathBuf {
    if input.platform == "win32" {
        let local_app_data = input
            .env
            .get("LOCALAPPDATA")
            .or_else(|| input.env.get("APPDATA"))
            .map(PathBuf::from);
        let base = local_app_data.unwrap_or_else(|| {
            input.homedir.join("AppData").join("Local")
        });
        return base.join(input.cache_dir_name).join("bin");
    }
    let base = input
        .env
        .get("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| input.homedir.join(".cache"));
    base.join(input.cache_dir_name).join("bin")
}
