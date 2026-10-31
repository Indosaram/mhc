use std::path::{Path, PathBuf};

fn valid(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|metadata| metadata.len() > 10_000)
}

pub struct BinaryResolver {
    pub source_path: PathBuf,
    pub cache: PathBuf,
    pub path: Option<std::ffi::OsString>,
    pub platform_key: String,
    pub version: String,
    pub offline: bool,
    /// Upstream module-level `resolvedCliPath`.
    resolved: std::sync::Mutex<Option<PathBuf>>,
    /// Upstream `initPromise`: concurrent `getAstGrepPath()` callers share one resolution.
    init: tokio::sync::Mutex<()>,
}
impl BinaryResolver {
    pub fn new(
        source_path: PathBuf,
        cache: PathBuf,
        path: Option<std::ffi::OsString>,
        platform_key: String,
        offline: bool,
    ) -> Self {
        let version = source_path
            .parent()
            .and_then(|parent| {
                parent.ancestors().find_map(|directory| {
                    let bytes =
                        std::fs::read(directory.join("node_modules/@ast-grep/cli/package.json"))
                            .ok()?;
                    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
                    value.get("version")?.as_str().map(str::to_owned)
                })
            })
            .unwrap_or_else(|| crate::downloader::DEFAULT_AST_GREP_VERSION.to_owned());
        Self {
            source_path,
            cache,
            path,
            platform_key,
            version,
            offline,
            resolved: std::sync::Mutex::new(None),
            init: tokio::sync::Mutex::new(()),
        }
    }
    fn resolved_existing(&self) -> Option<PathBuf> {
        self.resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .filter(|path| path.exists())
            .cloned()
    }
    fn store(&self, path: &Path) {
        *self
            .resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path.to_owned());
    }
    /// Upstream `findSgCliPathSync()` against this resolver's cache, package root and PATH.
    pub fn find_sync(&self) -> Option<PathBuf> {
        let platform = self.platform_key.split('-').next()?;
        let cached = crate::downloader::cached_binary_path(&self.cache, platform);
        find_sg_cli_path(&self.source_path, cached.as_deref(), self.path.as_deref())
    }
    /// Upstream `getSgCliPath()`: the remembered path while it exists, else a synchronous search.
    pub fn get_sg_cli_path(&self) -> Option<PathBuf> {
        if let Some(path) = self.resolved_existing() {
            return Some(path);
        }
        let path = self.find_sync()?;
        self.store(&path);
        Some(path)
    }
    /// Upstream `setSgCliPath(path)`.
    pub fn set_sg_cli_path(&self, path: PathBuf) {
        *self
            .resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(path);
    }
    /// Upstream `getAstGrepPath()`: synchronous search, then `ensureAstGrepBinary()`. A failed
    /// resolution leaves the remembered path untouched, as upstream only assigns on success.
    pub async fn resolve(&self) -> Option<PathBuf> {
        if let Some(path) = self.resolved_existing() {
            return Some(path);
        }
        let _init = self.init.lock().await;
        if let Some(path) = self.resolved_existing() {
            return Some(path);
        }
        let path = match self.find_sync() {
            Some(path) => Some(path),
            None => {
                crate::downloader::ensure_ast_grep_binary(
                    &self.cache,
                    &self.platform_key,
                    &self.version,
                    self.offline,
                )
                .await
            }
        }?;
        self.store(&path);
        Some(path)
    }
    /// Upstream `startBackgroundInit()`: resolve (and maybe download) without awaiting.
    /// Requires a running tokio runtime; without one it is a no-op like a dropped promise.
    pub fn start_background_init(self: &std::sync::Arc<Self>) {
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            let resolver = std::sync::Arc::clone(self);
            handle.spawn(async move {
                let _path = resolver.resolve().await;
            });
        }
    }
    /// Upstream `isCliAvailable()`.
    pub fn is_cli_available(&self) -> bool {
        self.find_sync().is_some_and(|path| path.exists())
    }
    /// Upstream `ensureCliAvailable()`.
    pub async fn ensure_cli_available(&self) -> bool {
        self.resolve().await.is_some_and(|path| path.exists())
    }
    /// Upstream `resetResolvedPathForTests()`.
    pub fn reset_resolved_path_for_tests(&self) {
        *self
            .resolved
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

pub fn find_sg_cli_path(
    source_path: &Path,
    cache: Option<&Path>,
    path: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    if let Some(cache) = cache.filter(|path| valid(path)) {
        return Some(cache.to_owned());
    }
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
        let directory = source_path
            .parent()?
            .ancestors()
            .map(|parent| parent.join("node_modules").join(package))
            .find(|directory| directory.join("package.json").exists())?;
        let binary = directory.join(binary);
        valid(&binary).then_some(binary)
    };
    if let Some(binary) = package_binary("@ast-grep/cli", name) {
        return Some(binary);
    }
    if let Some(package) = platform_package
        && let Some(binary) = package_binary(
            package,
            if cfg!(windows) {
                "ast-grep.exe"
            } else {
                "ast-grep"
            },
        )
    {
        return Some(binary);
    }
    if let Some(path) = path.filter(|value| !value.is_empty()) {
        for directory in std::env::split_paths(path) {
            for suffix in if cfg!(windows) {
                &["", ".exe"][..]
            } else {
                &[""][..]
            } {
                let candidate = directory.join(format!("{name}{suffix}"));
                if valid(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    if cfg!(target_os = "macos") {
        for path in ["/opt/homebrew/bin/sg", "/usr/local/bin/sg"] {
            if valid(Path::new(path)) {
                return Some(PathBuf::from(path));
            }
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
        std::fs::write(package.join("package.json"), r#"{"version":"0.41.1"}"#)
            .expect("write package version");
        let binary = package.join(if cfg!(windows) { "sg.exe" } else { "sg" });
        std::fs::write(&binary, vec![0; 10_001]).expect("write package binary");
        let resolver = BinaryResolver::new(
            root.path().join("source.rs"),
            root.path().join("cache"),
            None,
            "linux-x64".into(),
            true,
        );
        assert_eq!(resolver.version, "0.41.1");
        assert_eq!(resolver.resolve().await, Some(binary.clone()));
        assert_eq!(resolver.resolve().await, Some(binary));
    }
    #[test]
    fn cache_precedes_package() {
        let root = tempfile::tempdir().expect("create binary fixture");
        let cache = root.path().join("cached-sg");
        std::fs::write(&cache, vec![0; 10_001]).expect("write cached binary");
        assert_eq!(
            find_sg_cli_path(&root.path().join("source.rs"), Some(&cache), None),
            Some(cache)
        );
    }
    #[tokio::test]
    async fn removed_cached_path_resolves_package_again() {
        let root = tempfile::tempdir().expect("create resolver fixture");
        let cache = root.path().join("cache");
        let package = root.path().join("node_modules/@ast-grep/cli");
        std::fs::create_dir_all(&cache).expect("create cache");
        std::fs::create_dir_all(&package).expect("create package");
        std::fs::write(package.join("package.json"), "{}").expect("package metadata");
        let name = if cfg!(windows) { "sg.exe" } else { "sg" };
        let cached = cache.join(name);
        let installed = package.join(name);
        std::fs::write(&cached, vec![0; 10_001]).expect("cached binary");
        std::fs::write(&installed, vec![0; 10_001]).expect("installed binary");
        let platform = if cfg!(windows) {
            "win32-x64"
        } else {
            "linux-x64"
        };
        let resolver = BinaryResolver::new(
            root.path().join("source.rs"),
            cache,
            None,
            platform.into(),
            true,
        );
        assert_eq!(resolver.resolve().await, Some(cached.clone()));
        std::fs::remove_file(cached).expect("remove stale binary");
        assert_eq!(resolver.resolve().await, Some(installed));
    }
    // macOS also probes Homebrew paths, which may hold a real sg.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn set_path_and_reset_follow_upstream_state() {
        let root = tempfile::tempdir().expect("create resolver fixture");
        let manual = root.path().join("manual-sg");
        std::fs::write(&manual, b"x").expect("write manual binary");
        let resolver = BinaryResolver::new(
            root.path().join("source.rs"),
            root.path().join("cache"),
            None,
            "linux-x64".into(),
            true,
        );
        assert_eq!(resolver.get_sg_cli_path(), None);
        assert!(!resolver.is_cli_available());
        resolver.set_sg_cli_path(manual.clone());
        assert_eq!(resolver.get_sg_cli_path(), Some(manual.clone()));
        assert!(resolver.ensure_cli_available().await);
        resolver.reset_resolved_path_for_tests();
        assert_eq!(resolver.get_sg_cli_path(), None);
        assert!(!resolver.ensure_cli_available().await);
    }
    #[test]
    fn small_binary_is_rejected() {
        let root = tempfile::tempdir().expect("create binary fixture");
        let cache = root.path().join("cached-sg");
        std::fs::write(&cache, vec![0; 10_000]).expect("write undersized binary");
        assert_ne!(
            find_sg_cli_path(&root.path().join("source.rs"), Some(&cache), None),
            Some(cache)
        );
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
        assert_eq!(
            find_sg_cli_path(
                &root.path().join("source.rs"),
                None,
                Some(root.path().as_os_str())
            ),
            Some(binary)
        );
    }
}
