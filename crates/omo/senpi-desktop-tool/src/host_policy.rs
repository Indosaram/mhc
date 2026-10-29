//! Host platform support policy matching upstream `src/host-policy.ts`.

/// Whether this host may register the `computer` tool.
/// Windows arm64 counts as supported; when no engine prebuild exists for it,
/// the engine locator's `native-unavailable` diagnostic surfaces on first use.
pub fn is_supported_host(platform: &str) -> bool {
    matches!(platform, "darwin" | "linux" | "win32")
}

/// The stop chord armed when `computer.stopHotkey` is unset:
/// `ctrl+alt+cmd+escape` on macOS, `ctrl+alt+shift+escape` on others.
pub fn default_stop_hotkey(platform: &str) -> &'static str {
    if platform == "darwin" {
        "ctrl+alt+cmd+escape"
    } else {
        "ctrl+alt+shift+escape"
    }
}
