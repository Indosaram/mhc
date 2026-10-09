use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use comment_checker_core::*;

const RELEASE_BASE: &str =
    "https://github.com/code-yeongyu/go-claude-code-comment-checker/releases/download/v0.8.0/";

#[test]
fn release_descriptor_given_the_pinned_version_when_read_then_it_is_0_8_0() {
    assert_eq!(COMMENT_CHECKER_RELEASE_VERSION, "0.8.0");
}

#[test]
fn release_descriptor_resolves_supported_platform_assets() {
    let cases = [
        (
            "darwin",
            "arm64",
            "comment-checker_v0.8.0_darwin_arm64.tar.gz",
            CommentCheckerArchiveExtension::TarGz,
        ),
        (
            "darwin",
            "x64",
            "comment-checker_v0.8.0_darwin_amd64.tar.gz",
            CommentCheckerArchiveExtension::TarGz,
        ),
        (
            "linux",
            "arm64",
            "comment-checker_v0.8.0_linux_arm64.tar.gz",
            CommentCheckerArchiveExtension::TarGz,
        ),
        (
            "linux",
            "x64",
            "comment-checker_v0.8.0_linux_amd64.tar.gz",
            CommentCheckerArchiveExtension::TarGz,
        ),
        (
            "win32",
            "x64",
            "comment-checker_v0.8.0_windows_amd64.zip",
            CommentCheckerArchiveExtension::Zip,
        ),
    ];

    for (platform, arch, asset_name, ext) in cases {
        let asset = resolve_comment_checker_release_asset(platform, arch, None)
            .expect("supported platform asset");
        let parts: Vec<&str> = asset_name.split('_').collect();
        let expected_os = parts[2];
        let expected_arch = parts[3].split('.').next().unwrap();
        assert_eq!(asset.os, expected_os);
        assert_eq!(asset.arch, expected_arch);
        assert_eq!(asset.ext, ext);
        assert_eq!(asset.asset_name, asset_name);
        assert_eq!(asset.url, format!("{RELEASE_BASE}{asset_name}"));
    }
}

#[test]
fn release_descriptor_given_unsupported_platform_when_resolving_asset_then_there_is_none() {
    assert!(resolve_comment_checker_release_asset("freebsd", "x64", None).is_none());
    assert!(resolve_comment_checker_release_asset("win32", "arm64", None).is_none());
}

#[test]
fn release_descriptor_given_different_version_when_resolving_asset_then_version_threads() {
    let asset = resolve_comment_checker_release_asset("linux", "x64", Some("9.9.9"))
        .expect("resolved 9.9.9 asset");
    assert_eq!(
        asset.asset_name,
        "comment-checker_v9.9.9_linux_amd64.tar.gz"
    );
    assert!(asset.url.contains("/releases/download/v9.9.9/"));
}

#[test]
fn release_descriptor_given_each_platform_when_naming_binary_then_only_win32_carries_exe() {
    assert_eq!(comment_checker_binary_name("win32"), "comment-checker.exe");
    assert_eq!(comment_checker_binary_name("darwin"), "comment-checker");
    assert_eq!(comment_checker_binary_name("linux"), "comment-checker");
}

#[test]
fn release_descriptor_given_xdg_cache_home_on_posix_when_locating_cache_then_it_is_xdg_name_bin() {
    let mut env = BTreeMap::new();
    env.insert("XDG_CACHE_HOME".to_string(), "/xdg".to_string());
    let dir = comment_checker_cache_dir(&CommentCheckerCacheDirInput {
        platform: "linux",
        env: &env,
        homedir: Path::new("/home/u"),
        cache_dir_name: "oh-my-opencode",
    });
    assert_eq!(dir, PathBuf::from("/xdg/oh-my-opencode/bin"));
}

#[test]
fn release_descriptor_given_no_xdg_cache_home_on_posix_when_locating_cache_then_falls_back_to_dot_cache()
{
    let env = BTreeMap::new();
    let dir = comment_checker_cache_dir(&CommentCheckerCacheDirInput {
        platform: "darwin",
        env: &env,
        homedir: Path::new("/Users/u"),
        cache_dir_name: "oh-my-opencode",
    });
    assert_eq!(dir, PathBuf::from("/Users/u/.cache/oh-my-opencode/bin"));
}

#[test]
fn release_descriptor_given_win32_when_locating_cache_then_localappdata_wins_then_appdata_then_appdata_local()
{
    let name = "oh-my-opencode";
    let homedir = Path::new("H");

    let mut env = BTreeMap::new();
    env.insert("LOCALAPPDATA".to_string(), "L".to_string());
    env.insert("APPDATA".to_string(), "A".to_string());
    assert_eq!(
        comment_checker_cache_dir(&CommentCheckerCacheDirInput {
            platform: "win32",
            env: &env,
            homedir,
            cache_dir_name: name,
        }),
        PathBuf::from("L").join(name).join("bin")
    );

    let mut env = BTreeMap::new();
    env.insert("APPDATA".to_string(), "A".to_string());
    assert_eq!(
        comment_checker_cache_dir(&CommentCheckerCacheDirInput {
            platform: "win32",
            env: &env,
            homedir,
            cache_dir_name: name,
        }),
        PathBuf::from("A").join(name).join("bin")
    );

    let env = BTreeMap::new();
    assert_eq!(
        comment_checker_cache_dir(&CommentCheckerCacheDirInput {
            platform: "win32",
            env: &env,
            homedir,
            cache_dir_name: name,
        }),
        homedir
            .join("AppData")
            .join("Local")
            .join(name)
            .join("bin")
    );
}
