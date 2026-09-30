//! Port of senpi `packages/tui/src/native-module-path.ts`.

use std::path::{Path, PathBuf};

pub const TUI_PACKAGE_NAME: &str = "@earendil-works/pi-tui";

type ResolvePackage<'a> = &'a dyn Fn(&str) -> Result<String, String>;

#[derive(Default)]
pub struct NativeModuleCandidateOptions<'a> {
    /// Path of the calling module (senpi: `import.meta.url`); defaults to the executable.
    pub module_path: Option<PathBuf>,
    pub exec_path: Option<PathBuf>,
    /// Resolves the installed TUI package entry. Rust binaries have no package resolver, so
    /// the default is "not installed" (the standalone-binary path).
    pub resolve_package: Option<ResolvePackage<'a>>,
}

fn current_exe() -> PathBuf {
    std::env::current_exe().unwrap_or_default()
}

fn parent(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// Lexical `path.join` normalization (`..` pops a component).
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn get_native_module_candidates(
    native_path: &Path,
    options: &NativeModuleCandidateOptions<'_>,
) -> Vec<PathBuf> {
    let module_dir = parent(&options.module_path.clone().unwrap_or_else(current_exe));
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Some(resolve) = options.resolve_package {
        // A resolver that answers with the bare specifier would yield a relative candidate.
        if let Ok(entry) = resolve(TUI_PACKAGE_NAME) {
            let entry = PathBuf::from(entry);
            if entry.is_absolute() {
                candidates.push(normalize(&parent(&entry).join("..").join(native_path)));
            }
        }
    }

    let exec_path = options.exec_path.clone().unwrap_or_else(current_exe);
    for candidate in [
        normalize(&module_dir.join("..").join(native_path)),
        normalize(&module_dir.join(native_path)),
        normalize(&parent(&exec_path).join(native_path)),
    ] {
        candidates.push(candidate);
    }
    let mut seen = std::collections::HashSet::new();
    candidates.retain(|c| seen.insert(c.clone()));
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    fn virt(parts: &[&str]) -> PathBuf {
        let mut p = std::env::current_dir().expect("cwd").join("virtual");
        for part in parts {
            p.push(part);
        }
        p
    }

    #[test]
    fn resolves_native_helpers_from_the_installed_tui_package_when_the_module_is_bundled_elsewhere()
    {
        let package_root = virt(&["node_modules", "@earendil-works", "pi-tui"]);
        let bundled_module = virt(&["pi-coding-agent", "dist", "bundle", "chunks", "chunk.js"]);
        let native = Path::new("native/win32/prebuilds/win32-arm64/win32-platform.node");
        let entry = package_root.join("dist").join("index.js");
        let resolve = |specifier: &str| {
            assert_eq!(specifier, "@earendil-works/pi-tui");
            Ok(entry.to_string_lossy().into_owned())
        };
        let candidates = get_native_module_candidates(
            native,
            &NativeModuleCandidateOptions {
                module_path: Some(bundled_module.clone()),
                exec_path: Some(virt(&["node", "node.exe"])),
                resolve_package: Some(&resolve),
            },
        );
        assert_eq!(candidates[0], package_root.join(native));
        assert!(candidates.contains(&normalize(&parent(&bundled_module).join("..").join(native))));
    }

    #[test]
    fn ignores_a_resolver_that_answers_with_the_bare_specifier_instead_of_a_path() {
        let bundled_module = virt(&["pi-coding-agent", "dist", "bundle", "chunks", "chunk.js"]);
        let exec_path = virt(&["node", "node.exe"]);
        let native = Path::new("native/darwin/prebuilds/darwin-arm64/darwin-platform.node");
        let resolve = |specifier: &str| Ok(specifier.to_string());
        let candidates = get_native_module_candidates(
            native,
            &NativeModuleCandidateOptions {
                module_path: Some(bundled_module.clone()),
                exec_path: Some(exec_path.clone()),
                resolve_package: Some(&resolve),
            },
        );
        assert!(candidates.iter().all(|c| c.is_absolute()), "{candidates:?}");
        assert_eq!(
            candidates,
            vec![
                normalize(&parent(&bundled_module).join("..").join(native)),
                parent(&bundled_module).join(native),
                parent(&exec_path).join(native),
            ]
        );
    }

    #[test]
    fn keeps_standalone_binary_fallbacks_when_the_tui_package_is_unavailable() {
        let bundled_module = virt(&["pi", "bundle", "chunks", "chunk.js"]);
        let exec_path = virt(&["pi", "pi.exe"]);
        let native = Path::new("native/darwin/prebuilds/darwin-arm64/darwin-platform.node");
        let resolve = |_: &str| Err("not installed".to_string());
        let candidates = get_native_module_candidates(
            native,
            &NativeModuleCandidateOptions {
                module_path: Some(bundled_module.clone()),
                exec_path: Some(exec_path.clone()),
                resolve_package: Some(&resolve),
            },
        );
        assert_eq!(
            candidates,
            vec![
                normalize(&parent(&bundled_module).join("..").join(native)),
                parent(&bundled_module).join(native),
                parent(&exec_path).join(native),
            ]
        );
    }
}
