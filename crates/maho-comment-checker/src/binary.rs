use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// npm package that ships the native checker.
pub const PACKAGE_NAME: &str = "@code-yeongyu/comment-checker";
/// Environment override naming the checker executable.
pub const BINARY_ENV: &str = "COMMENT_CHECKER_BINARY";

/// Executable file name on this platform.
pub fn binary_name() -> &'static str {
    if cfg!(windows) {
        "comment-checker.exe"
    } else {
        "comment-checker"
    }
}

/// `<platform>-<arch>` in Node's naming (`linux-x64`, `darwin-arm64`, `win32-x64`).
pub fn platform_key() -> String {
    let platform = if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("{platform}-{arch}")
}

/// Inputs for [`resolve_binary`].
#[derive(Clone, Debug, Default)]
pub struct BinaryLookup {
    /// A file or directory whose ancestors are searched for `node_modules/<package>`.
    pub start: Option<PathBuf>,
    /// Value of [`BINARY_ENV`]; `None` skips the override.
    pub override_path: Option<OsString>,
    /// `PATH` to search last; `None` skips the search.
    pub path_env: Option<OsString>,
}

impl BinaryLookup {
    /// Lookup from `start` using this process's environment.
    pub fn from_env(start: Option<PathBuf>) -> Self {
        Self {
            start,
            override_path: std::env::var_os(BINARY_ENV),
            path_env: std::env::var_os("PATH"),
        }
    }
}

/// Finds the checker: the override, then the installed npm package nearest `start`, then `PATH`.
pub fn resolve_binary(lookup: &BinaryLookup) -> Option<PathBuf> {
    if let Some(path) = lookup
        .override_path
        .as_ref()
        .filter(|value| !value.is_empty())
    {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Some(found) = lookup.start.as_deref().and_then(resolve_package_binary) {
        return Some(found);
    }
    let path_env = lookup.path_env.as_ref()?;
    std::env::split_paths(path_env)
        .map(|directory| directory.join(binary_name()))
        .find(|candidate| candidate.is_file())
}

/// Package lookup from the nearest ancestor of `start` holding `node_modules/<package>/package.json`:
/// the per-platform package's `bin/`, then the package's `vendor/<platform-arch>/`, then its `bin/`.
pub fn resolve_package_binary(start: &Path) -> Option<PathBuf> {
    let name = binary_name();
    let key = platform_key();
    let first = if start.is_dir() {
        Some(start)
    } else {
        start.parent()
    };
    for directory in first.into_iter().flat_map(Path::ancestors) {
        let modules = directory.join("node_modules");
        let package = modules.join(PACKAGE_NAME);
        if !package.join("package.json").is_file() {
            continue;
        }
        let platform_package = modules.join(format!("{PACKAGE_NAME}-{key}"));
        let candidates = [
            platform_package.join("bin").join(name),
            package.join("vendor").join(&key).join(name),
            package.join("bin").join(name),
        ];
        let platform_ok = platform_package.join("package.json").is_file();
        return candidates
            .into_iter()
            .enumerate()
            .filter(|(index, _)| *index != 0 || platform_ok)
            .map(|(_, path)| path)
            .find(|path| path.is_file());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, "x").expect("write");
    }

    #[test]
    fn package_order_is_platform_vendor_bin() {
        let root = tempfile::tempdir().expect("tempdir");
        let modules = root.path().join("node_modules");
        let package = modules.join(PACKAGE_NAME);
        touch(&package.join("package.json"));
        let start = root.path().join("src/index.ts");
        assert_eq!(resolve_package_binary(&start), None);
        let bin = package.join("bin").join(binary_name());
        touch(&bin);
        assert_eq!(resolve_package_binary(&start), Some(bin));
        let vendor = package
            .join("vendor")
            .join(platform_key())
            .join(binary_name());
        touch(&vendor);
        assert_eq!(resolve_package_binary(&start), Some(vendor.clone()));
        let platform = modules.join(format!("{PACKAGE_NAME}-{}", platform_key()));
        let platform_bin = platform.join("bin").join(binary_name());
        touch(&platform_bin);
        assert_eq!(resolve_package_binary(&start), Some(vendor));
        touch(&platform.join("package.json"));
        assert_eq!(resolve_package_binary(&start), Some(platform_bin));
    }

    #[test]
    fn override_then_package_then_path() {
        let root = tempfile::tempdir().expect("tempdir");
        let on_path = root.path().join("path-dir").join(binary_name());
        touch(&on_path);
        let mut lookup = BinaryLookup {
            start: Some(root.path().join("nowhere/x.ts")),
            override_path: None,
            path_env: Some(std::env::join_paths([on_path.parent().expect("dir")]).expect("join")),
        };
        assert_eq!(resolve_binary(&lookup), Some(on_path.clone()));
        let package_bin = root
            .path()
            .join("node_modules")
            .join(PACKAGE_NAME)
            .join("bin")
            .join(binary_name());
        touch(
            &root
                .path()
                .join("node_modules")
                .join(PACKAGE_NAME)
                .join("package.json"),
        );
        touch(&package_bin);
        assert_eq!(resolve_binary(&lookup), Some(package_bin));
        let explicit = root.path().join("explicit");
        touch(&explicit);
        lookup.override_path = Some(explicit.clone().into_os_string());
        assert_eq!(resolve_binary(&lookup), Some(explicit));
        lookup.override_path = Some(root.path().join("missing").into_os_string());
        lookup.start = None;
        assert_eq!(resolve_binary(&lookup), Some(on_path));
        lookup.path_env = None;
        assert_eq!(resolve_binary(&lookup), None);
    }
}
