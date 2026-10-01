use std::path::{Path, PathBuf};

fn valid(path: &Path) -> bool { path.metadata().is_ok_and(|metadata| metadata.len() > 10_000) }

pub struct BinaryResolver {
    pub source_path: PathBuf,
    pub cache: PathBuf,
    pub path: Option<std::ffi::OsString>,
    pub platform_key: String,
    pub version: String,
    pub offline: bool,
    resolved: tokio::sync::Mutex<Option<PathBuf>>,
}
impl BinaryResolver {
    pub fn new(source_path: PathBuf, cache: PathBuf, path: Option<std::ffi::OsString>, platform_key: String, offline: bool) -> Self {
        let version = source_path.parent().and_then(|parent| parent.ancestors().find_map(|directory| {
            let bytes = std::fs::read(directory.join("node_modules/@ast-grep/cli/package.json")).ok()?;
            let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
            value.get("version")?.as_str().map(str::to_owned)
        })).unwrap_or_else(|| crate::downloader::DEFAULT_AST_GREP_VERSION.to_owned());
        Self { source_path, cache, path, platform_key, version, offline, resolved: tokio::sync::Mutex::new(None) }
    }
    pub async fn resolve(&self) -> Option<PathBuf> {
        let mut resolved = self.resolved.lock().await;
        if let Some(path) = resolved.as_ref().filter(|path| path.exists()) { return Some(path.clone()); }
        let platform = self.platform_key.split('-').next()?;
        let cached = crate::downloader::cached_binary_path(&self.cache, platform);
        let path = if let Some(path) = find_sg_cli_path(&self.source_path, cached.as_deref(), self.path.as_deref()) { Some(path) }
            else { crate::downloader::ensure_ast_grep_binary(&self.cache, &self.platform_key, &self.version, self.offline).await };
        *resolved = path.clone();
        path
    }
}

pub fn find_sg_cli_path(source_path: &Path, cache: Option<&Path>, path: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    if let Some(cache) = cache.filter(|path| valid(path)) { return Some(cache.to_owned()); }
    let name = if cfg!(windows) { "sg.exe" } else { "sg" };
    let platform_package = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("@ast-grep/cli-darwin-arm64"),
        ("macos", "x86_64") => Some("@ast-grep/cli-darwin-x64"),
        ("linux", "aarch64") => Some("@ast-grep/cli-linux-arm64-gnu"),
        ("linux", "x86_64") => Some("@ast-grep/cli-linux-x64-gnu"),
        ("windows", "x86_64") => Some("@ast-grep/cli-win32-x64-msvc"),
        ("windows", "aarch64") => Some("@ast-grep/cli-win32-arm64-msvc"),
        ("windows", "x86") => Some("@ast-grep/cli-win32-ia32-msvc"),
        _ => None,
    };
    let package_binary = |package: &str, binary: &str| {
        let directory = source_path.parent()?.ancestors().map(|parent| parent.join("node_modules").join(package)).find(|directory| directory.join("package.json").exists())?;
        let binary = directory.join(binary);
        valid(&binary).then_some(binary)
    };
    if let Some(binary) = package_binary("@ast-grep/cli", name) { return Some(binary); }
    if let Some(package) = platform_package
        && let Some(binary) = package_binary(package, if cfg!(windows) { "ast-grep.exe" } else { "ast-grep" }) { return Some(binary); }
    if let Some(path) = path.filter(|value| !value.is_empty()) {
        for directory in std::env::split_paths(path) {
            for suffix in if cfg!(windows) { &["", ".exe"][..] } else { &[""][..] } {
                let candidate = directory.join(format!("{name}{suffix}"));
                if valid(&candidate) { return Some(candidate); }
            }
        }
    }
    if cfg!(target_os = "macos") {
        for path in ["/opt/homebrew/bin/sg", "/usr/local/bin/sg"] {
            if valid(Path::new(path)) { return Some(PathBuf::from(path)); }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn lazy_resolution_reuses_valid_package_binary() {
        let root = tempfile::tempdir().expect("create resolver fixture");
        let package = root.path().join("node_modules/@ast-grep/cli");
        std::fs::create_dir_all(&package).expect("create resolver package");
        std::fs::write(package.join("package.json"), r#"{"version":"0.41.1"}"#).expect("write package version");
        let binary = package.join(if cfg!(windows) { "sg.exe" } else { "sg" });
        std::fs::write(&binary, vec![0; 10_001]).expect("write package binary");
        let resolver = BinaryResolver::new(root.path().join("source.rs"), root.path().join("cache"), None, "linux-x64".into(), true);
        assert_eq!(resolver.version, "0.41.1");
        assert_eq!(resolver.resolve().await, Some(binary.clone()));
        assert_eq!(resolver.resolve().await, Some(binary));
    }
    #[test]
    fn cache_precedes_package() {
        let root = tempfile::tempdir().expect("create binary fixture");
        let cache = root.path().join("cached-sg");
        std::fs::write(&cache, vec![0; 10_001]).expect("write cached binary");
        assert_eq!(find_sg_cli_path(&root.path().join("source.rs"), Some(&cache), None), Some(cache));
    }
    #[test]
    fn small_binary_is_rejected() {
        let root = tempfile::tempdir().expect("create binary fixture");
        let cache = root.path().join("cached-sg");
        std::fs::write(&cache, vec![0; 10_000]).expect("write undersized binary");
        assert_ne!(find_sg_cli_path(&root.path().join("source.rs"), Some(&cache), None), Some(cache));
    }
    #[test]
    fn package_precedes_path() {
        let root = tempfile::tempdir().expect("create binary fixture");
        let package = root.path().join("node_modules/@ast-grep/cli");
        std::fs::create_dir_all(&package).expect("create package directory");
        std::fs::write(package.join("package.json"), "{}").expect("write package metadata");
        let name = if cfg!(windows) { "sg.exe" } else { "sg" };
        let binary = package.join(name);
        std::fs::write(&binary, vec![0; 10_001]).expect("write package binary");
        std::fs::write(root.path().join(name), vec![0; 10_001]).expect("write PATH binary");
        assert_eq!(find_sg_cli_path(&root.path().join("source.rs"), None, Some(root.path().as_os_str())), Some(binary));
    }
}
