//! MIT profile (`--no-default-features`): browser automation and computer use from `maho-browser`
//! (ported from the MIT omowright, pi-cua-integration and oh-my-pi sources). Mounts
//! `<builtin:browser>`: the `browser` tool (Chromium over the DevTools protocol: navigation,
//! accessibility snapshots with element refs, clicks, typing, screenshots, JavaScript) and the
//! `computer` tool (coordinate-level control of a browser viewport or, through the vendored desktop
//! daemon, the host desktop or a Cua sandbox). The browser and the daemon start on first use and
//! stop at session shutdown. The `omo` build does not mount these tools.
use maho_browser::BrowserExtension;

/// The browser/computer extension configured from the environment (`$MAHO_BROWSER_PATH`,
/// `$MAHO_BROWSER_HEADLESS`, `$MAHO_BROWSER_CDP_URL`, `$MAHO_BROWSER_TRANSPORT`,
/// `$MAHO_COMPUTER_PYTHON`; see docs/MIT-PROFILE.md).
pub fn extension() -> BrowserExtension {
    let mut settings = BrowserExtension::from_env();
    settings.daemon_dir =
        std::path::PathBuf::from(maho_core::config::get_agent_dir()).join("computer-use");
    settings
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_desktop_daemon_lives_under_the_agent_dir() {
        let settings = super::extension();
        assert!(settings.daemon_dir.ends_with("computer-use"));
        assert_eq!(maho_browser::TOOL_NAMES, &["browser", "computer"]);
    }
}
