//! Port of interactive/tmux-setup.ts.

#[derive(Debug, Clone, Default)]
pub struct TmuxSetupCheck<'a> {
    pub extended_keys: Option<&'a str>,
    pub extended_keys_format: Option<&'a str>,
    pub images_enabled: bool,
    pub outer_kitty_capable: bool,
    pub allow_passthrough: Option<&'a str>,
    pub focus_events: Option<&'a str>,
    pub version: Option<&'a str>,
}

pub fn build_tmux_setup_warning(check: &TmuxSetupCheck<'_>) -> Option<String> {
    let mut recommendations: Vec<(String, &str)> = Vec::new();
    if !matches!(check.extended_keys, Some("on" | "always")) {
        recommendations.push(("set -g extended-keys on".into(), "modified Enter keys (Shift+Enter, ...)"));
    }
    if check.extended_keys_format == Some("xterm") {
        recommendations.push(("set -g extended-keys-format csi-u".into(), "Pi works best with csi-u"));
    }
    if !check.images_enabled && check.outer_kitty_capable {
        let version = check.version.and_then(|value| {
            let (major, tail) = value.split_once('.')?;
            let minor: String = tail.chars().take_while(char::is_ascii_digit).collect();
            Some((major.parse::<u64>().ok()?, minor.parse::<u64>().ok()?))
        }).unwrap_or((0, 0));
        if check.version.is_some_and(|version| !version.is_empty()) && version < (3, 3) {
            recommendations.push((format!("upgrade tmux (current {})", check.version.unwrap_or_default()), "inline images need tmux >= 3.3"));
        } else {
            if !matches!(check.allow_passthrough, Some("1" | "on" | "all")) {
                recommendations.push(("set -g allow-passthrough on".into(), "inline images (Kitty graphics)"));
            }
            if !matches!(check.focus_events, Some("1" | "on")) {
                recommendations.push(("set -g focus-events on".into(), "repaint images after pane activation"));
            }
        }
    }
    let width = recommendations.iter().map(|(setting, _)| setting.len()).max()?;
    let lines: Vec<_> = recommendations.iter().map(|(setting, reason)| format!("  {setting:width$}  # {reason}")).collect();
    Some(format!("tmux is not fully configured. Add to ~/.tmux.conf and restart tmux:\n\n{}", lines.join("\n")))
}
