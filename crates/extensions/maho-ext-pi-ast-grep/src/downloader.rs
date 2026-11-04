use std::path::{Path, PathBuf};

pub const DEFAULT_AST_GREP_VERSION: &str = "0.41.1";
pub const PLATFORM_MAP: &[(&str, &str, &str)] = &[
    ("darwin-arm64", "aarch64", "apple-darwin"),
    ("darwin-x64", "x86_64", "apple-darwin"),
    ("linux-arm64", "aarch64", "unknown-linux-gnu"),
    ("linux-x64", "x86_64", "unknown-linux-gnu"),
    ("win32-x64", "x86_64", "pc-windows-msvc"),
    ("win32-arm64", "aarch64", "pc-windows-msvc"),
    ("win32-ia32", "i686", "pc-windows-msvc"),
];

pub fn cache_dir(home: &Path, platform: &str, cache_override: Option<&Path>) -> PathBuf {
    let base = cache_override.map(Path::to_owned).unwrap_or_else(|| {
        if platform == "win32" {
            home.join("AppData").join("Local")
        } else {
            home.join(".cache")
        }
    });
    base.join("pi-ast-grep").join("bin")
}
pub fn binary_name(platform: &str) -> &'static str {
    if platform == "win32" { "sg.exe" } else { "sg" }
}
pub fn cached_binary_path(cache: &Path, platform: &str) -> Option<PathBuf> {
    crate::binary_downloader::get_cached_binary_path(cache, binary_name(platform))
}

/// Node's `process.platform` for this build (`win32`, `darwin`, `linux`, ...).
pub fn current_platform() -> &'static str {
    match std::env::consts::OS {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    }
}
/// Node's `process.arch` for this build (`x64`, `arm64`, `ia32`, ...).
pub fn current_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        other => other,
    }
}
/// `${process.platform}-${process.arch}`, the `PLATFORM_MAP` key.
pub fn current_platform_key() -> String {
    format!("{}-{}", current_platform(), current_arch())
}
/// Reads one environment variable. The command handler takes this instead of `std::env` so a
/// test can inject `PI_OFFLINE`, `PATH` and the cache variables without mutating the process env.
pub type EnvReader = std::sync::Arc<dyn Fn(&str) -> Option<std::ffi::OsString> + Send + Sync>;
/// The process environment, read at call time.
pub fn process_env() -> EnvReader {
    std::sync::Arc::new(|key: &str| std::env::var_os(key))
}
/// Upstream `PI_OFFLINE === "1" || PI_OFFLINE === "true"`, read at call time.
pub fn is_offline() -> bool {
    is_offline_in(&*process_env())
}
pub fn is_offline_in(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    matches!(
        env("PI_OFFLINE")
            .as_deref()
            .and_then(std::ffi::OsStr::to_str),
        Some("1" | "true")
    )
}
/// Upstream `getCacheDir()`: `LOCALAPPDATA`/`APPDATA` on win32, `XDG_CACHE_HOME` elsewhere, else the home default.
pub fn get_cache_dir() -> PathBuf {
    get_cache_dir_in(&*process_env())
}
pub fn get_cache_dir_in(env: &dyn Fn(&str) -> Option<std::ffi::OsString>) -> PathBuf {
    let platform = current_platform();
    let home = env(if platform == "win32" {
        "USERPROFILE"
    } else {
        "HOME"
    })
    .map(PathBuf::from)
    .unwrap_or_default();
    let override_path = if platform == "win32" {
        env("LOCALAPPDATA").or_else(|| env("APPDATA"))
    } else {
        env("XDG_CACHE_HOME")
    }
    .map(PathBuf::from);
    cache_dir(&home, platform, override_path.as_deref())
}
/// Upstream `getBinaryName()`.
pub fn get_binary_name() -> &'static str {
    binary_name(current_platform())
}

pub async fn download_ast_grep(
    cache: &Path,
    platform_key: &str,
    version: &str,
    offline: bool,
) -> Option<PathBuf> {
    if offline {
        return None;
    }
    let (_, arch, os) = PLATFORM_MAP.iter().find(|entry| entry.0 == platform_key)?;
    let platform = platform_key.split('-').next()?;
    let binary = cache.join(binary_name(platform));
    if binary.exists() {
        return Some(binary);
    }
    let asset = format!("app-{arch}-{os}.zip");
    let url = format!("https://github.com/ast-grep/ast-grep/releases/download/{version}/{asset}");
    let archive = cache.join(asset);
    let operation = async {
        crate::binary_downloader::ensure_cache_dir(cache)?;
        crate::binary_downloader::download_archive(&url, &archive).await?;
        crate::binary_downloader::extract_zip_archive(&archive, cache)?;
        crate::binary_downloader::cleanup_archive(&archive)?;
        crate::binary_downloader::ensure_executable(&binary)?;
        Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
    };
    operation.await.ok()?;
    binary.exists().then_some(binary)
}

pub async fn ensure_ast_grep_binary(
    cache: &Path,
    platform_key: &str,
    version: &str,
    offline: bool,
) -> Option<PathBuf> {
    if offline {
        return None;
    }
    let platform = platform_key.split('-').next()?;
    if let Some(binary) = cached_binary_path(cache, platform) {
        return Some(binary);
    }
    download_ast_grep(cache, platform_key, version, false).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cache_override() {
        assert_eq!(
            cache_dir(
                Path::new("/home/test"),
                "linux",
                Some(Path::new("/tmp/cache"))
            ),
            Path::new("/tmp/cache/pi-ast-grep/bin")
        );
    }
    #[test]
    fn current_binary_name() {
        assert_eq!(
            get_binary_name(),
            if cfg!(windows) { "sg.exe" } else { "sg" }
        );
    }
    #[test]
    fn current_platform_key_is_node_shaped() {
        let key = current_platform_key();
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            assert_eq!(key, "linux-x64");
        }
        if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            assert_eq!(key, "darwin-arm64");
        }
        if cfg!(all(windows, target_arch = "x86_64")) {
            assert_eq!(key, "win32-x64");
        }
    }
    #[tokio::test]
    async fn unsupported_platform_returns_none_without_network() {
        let fixture = tempfile::tempdir().expect("create platform fixture");
        assert!(
            download_ast_grep(
                fixture.path(),
                "freebsd-x64",
                DEFAULT_AST_GREP_VERSION,
                false
            )
            .await
            .is_none()
        );
    }
    #[test]
    fn platform_binary() {
        assert_eq!(binary_name("linux"), "sg");
        assert_eq!(binary_name("win32"), "sg.exe");
    }
    #[test]
    fn platform_inventory() {
        assert_eq!(PLATFORM_MAP.len(), 7);
        for key in [
            "darwin-arm64",
            "darwin-x64",
            "linux-arm64",
            "linux-x64",
            "win32-x64",
            "win32-arm64",
            "win32-ia32",
        ] {
            assert!(PLATFORM_MAP.iter().any(|entry| entry.0 == key));
        }
    }
    #[test]
    fn pinned_version() {
        assert_eq!(DEFAULT_AST_GREP_VERSION, "0.41.1");
    }
    #[tokio::test]
    async fn offline_prevents_cache_and_network_resolution() {
        let fixture = tempfile::tempdir().expect("create offline fixture");
        std::fs::write(fixture.path().join("sg"), "cached").expect("write cache fixture");
        assert!(
            ensure_ast_grep_binary(fixture.path(), "linux-x64", DEFAULT_AST_GREP_VERSION, true)
                .await
                .is_none()
        );
    }
    #[tokio::test]
    async fn cached_binary_avoids_network() {
        let fixture = tempfile::tempdir().expect("create cache fixture");
        let binary = fixture.path().join("sg");
        std::fs::write(&binary, "cached").expect("write cached binary");
        assert_eq!(
            ensure_ast_grep_binary(fixture.path(), "linux-x64", DEFAULT_AST_GREP_VERSION, false)
                .await,
            Some(binary)
        );
    }
}
