use std::path::{Path, PathBuf};

pub const DEFAULT_AST_GREP_VERSION: &str = "0.41.1";
pub const PLATFORM_MAP: &[(&str, &str, &str)] = &[
    ("darwin-arm64", "aarch64", "apple-darwin"), ("darwin-x64", "x86_64", "apple-darwin"),
    ("linux-arm64", "aarch64", "unknown-linux-gnu"), ("linux-x64", "x86_64", "unknown-linux-gnu"),
    ("win32-x64", "x86_64", "pc-windows-msvc"), ("win32-arm64", "aarch64", "pc-windows-msvc"),
    ("win32-ia32", "i686", "pc-windows-msvc"),
];

pub fn cache_dir(home: &Path, platform: &str, cache_override: Option<&Path>) -> PathBuf {
    let base = cache_override.map(Path::to_owned).unwrap_or_else(|| if platform == "win32" { home.join("AppData/Local") } else { home.join(".cache") });
    base.join("pi-ast-grep/bin")
}
pub fn binary_name(platform: &str) -> &'static str { if platform == "win32" { "sg.exe" } else { "sg" } }
pub fn cached_binary_path(cache: &Path, platform: &str) -> Option<PathBuf> {
    let path = cache.join(binary_name(platform));
    path.exists().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn cache_override() { assert_eq!(cache_dir(Path::new("/home/test"), "linux", Some(Path::new("/tmp/cache"))), Path::new("/tmp/cache/pi-ast-grep/bin")); }
    #[test] fn platform_binary() { assert_eq!(binary_name("linux"), "sg"); assert_eq!(binary_name("win32"), "sg.exe"); }
    #[test] fn platform_inventory() { assert_eq!(PLATFORM_MAP.len(), 7); for key in ["darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64", "win32-x64", "win32-arm64", "win32-ia32"] { assert!(PLATFORM_MAP.iter().any(|entry| entry.0 == key)); } }
    #[test] fn pinned_version() { assert_eq!(DEFAULT_AST_GREP_VERSION, "0.41.1"); }
}
