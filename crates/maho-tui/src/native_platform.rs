//! Port of senpi `packages/tui/src/native-platform.ts`.
//!
//! senpi loads an optional prebuilt Node addon (`native/<platform>/prebuilds/...node`) for the
//! clipboard, Windows VT input and native modifier state; when no addon loads, every caller falls
//! back (no clipboard helper, no native Shift detection). maho has no Node addons, so the helper
//! is a registered Rust trait object and none is registered by default, which reproduces senpi's
//! "helper unavailable" path. See parity.d/6.md.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use crate::native_module_path::{NativeModuleCandidateOptions, get_native_module_candidates};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModifierKey {
    Shift,
    Command,
    Control,
    Option,
}

/// Clipboard access. `Ok(None)` = unavailable/no content; errors are transfer failures.
pub trait NativeClipboard: Send + Sync {
    fn get_text(&self) -> Result<Option<String>, String>;
    fn get_image(&self) -> Result<Option<Vec<u8>>, String>;
    fn set_text(&self, _text: &str) -> Option<Result<(), String>> {
        None
    }
}

pub trait NativePlatformHelper: NativeClipboard {
    fn enable_virtual_terminal_input(&self) -> Option<bool> {
        None
    }
    fn is_modifier_pressed(&self, _key: ModifierKey) -> Option<bool> {
        None
    }
}

type Helper = Arc<dyn NativePlatformHelper>;

static HELPERS: RwLock<Vec<(PathBuf, Helper)>> = RwLock::new(Vec::new());

/// Registers a helper for a native module path (the Rust stand-in for a loadable prebuild).
pub fn register_native_platform_helper(native_path: PathBuf, helper: Helper) {
    HELPERS
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push((native_path, helper));
}

fn native_path(platform: &str, arch: &str, suffix: &str) -> PathBuf {
    Path::new("native")
        .join(platform)
        .join("prebuilds")
        .join(format!("{platform}-{arch}"))
        .join(format!("{platform}-platform{suffix}.node"))
}

fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    }
}

fn load_native_platform_helper(platform: &str, suffix: &str) -> Option<Helper> {
    let arch = node_arch();
    if arch != "x64" && arch != "arm64" {
        return None;
    }
    let native = native_path(platform, arch, suffix);
    let candidates =
        get_native_module_candidates(&native, &NativeModuleCandidateOptions::default());
    let helpers = HELPERS
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    helpers
        .iter()
        .find(|(path, _)| path == &native || candidates.contains(path))
        .map(|(_, helper)| Arc::clone(helper))
}

/// Node's `process.platform` name for this build target.
pub fn node_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

pub fn get_native_platform_helper() -> Option<Helper> {
    let platform = node_platform();
    if platform != "darwin" && platform != "win32" {
        return None;
    }
    load_native_platform_helper(platform, "")
}

/// Loads a clipboard helper without opening the display until a read is requested.
pub fn get_native_clipboard() -> Option<Arc<dyn NativeClipboard>> {
    if node_platform() != "linux" {
        return get_native_platform_helper().map(|h| h as Arc<dyn NativeClipboard>);
    }
    crate::process_env::var("DISPLAY").filter(|d| !d.is_empty())?;
    load_native_platform_helper("linux", "-x11").map(|h| h as Arc<dyn NativeClipboard>)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::process_env::with_overrides;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct FakeHelper {
        available: AtomicBool,
        get_text_calls: AtomicUsize,
    }

    impl NativeClipboard for FakeHelper {
        fn get_text(&self) -> Result<Option<String>, String> {
            self.get_text_calls.fetch_add(1, Ordering::SeqCst);
            Ok(self
                .available
                .load(Ordering::SeqCst)
                .then(|| "X11".to_string()))
        }
        fn get_image(&self) -> Result<Option<Vec<u8>>, String> {
            Ok(None)
        }
        fn set_text(&self, _text: &str) -> Option<Result<(), String>> {
            Some(Ok(()))
        }
    }

    impl NativePlatformHelper for FakeHelper {}

    #[test]
    fn native_module_paths_follow_the_prebuild_layout() {
        assert_eq!(
            native_path("linux", "arm64", "-x11"),
            PathBuf::from("native/linux/prebuilds/linux-arm64/linux-platform-x11.node")
        );
        assert_eq!(
            native_path("win32", "x64", ""),
            PathBuf::from("native/win32/prebuilds/win32-x64/win32-platform.node")
        );
    }

    #[test]
    fn without_a_registered_helper_no_native_clipboard_or_helper_is_available() {
        assert!(get_native_platform_helper().is_none());
        with_overrides(&[("DISPLAY", Some(":0"))], || {
            assert!(get_native_clipboard().is_none())
        });
    }

    // The registry is process-wide; each test registers under its own suffix so parallel tests
    // cannot observe each other's helpers.
    #[test]
    fn uses_the_native_platform_helper_directly_as_the_clipboard_api() {
        let helper = Arc::new(FakeHelper {
            available: AtomicBool::new(true),
            get_text_calls: AtomicUsize::new(0),
        });
        register_native_platform_helper(native_path("darwin", node_arch(), "-direct"), helper);
        let clipboard: Arc<dyn NativeClipboard> =
            load_native_platform_helper("darwin", "-direct").expect("helper");
        assert_eq!(clipboard.get_text(), Ok(Some("X11".to_string())));
        assert_eq!(clipboard.get_image(), Ok(None));
        assert_eq!(clipboard.set_text("x"), Some(Ok(())));
    }

    #[test]
    fn linux_loads_x11_lazily_and_rechecks_display() {
        let helper = Arc::new(FakeHelper {
            available: AtomicBool::new(false),
            get_text_calls: AtomicUsize::new(0),
        });
        register_native_platform_helper(native_path("linux", node_arch(), "-lazy"), helper.clone());
        let load = |display: Option<&str>| {
            with_overrides(&[("DISPLAY", display)], || {
                crate::process_env::var("DISPLAY")
                    .filter(|d| !d.is_empty())
                    .and_then(|_| load_native_platform_helper("linux", "-lazy"))
            })
        };
        assert!(load(None).is_none());
        let clipboard = load(Some(":0")).expect("helper");
        assert_eq!(helper.get_text_calls.load(Ordering::SeqCst), 0);
        assert_eq!(clipboard.get_text(), Ok(None));
        helper.available.store(true, Ordering::SeqCst);
        assert_eq!(clipboard.get_text(), Ok(Some("X11".to_string())));
        assert_eq!(clipboard.get_image(), Ok(None));
        assert!(load(None).is_none());
        assert!(load(Some(":1")).is_some());
    }
}
