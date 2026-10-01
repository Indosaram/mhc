//! Port of senpi `packages/tui/src/tmux-image-capability.ts`.

use crate::tmux_image_probe::{TmuxAllowPassthrough, TmuxBaseState, TmuxImageState, TmuxSupportTier};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxKittyTerminal {
    Kitty,
    Ghostty,
    Warp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxKittyPlacement {
    Placeholder,
    Direct,
}

pub type TmuxKittyTerminalOverride = Option<TmuxKittyTerminal>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxImageDisabledReason {
    OutsideTmux,
    ProbeUnavailable,
    UnsupportedVersion,
    PassthroughOff,
    FocusEventsOff,
    MultipleClients,
    Nested,
    Hidden,
    UnknownClient,
    UnsafeDirectPlacement,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmuxImageDecision {
    Enabled {
        placement: TmuxKittyPlacement,
        terminal: TmuxKittyTerminal,
    },
    Disabled {
        reason: TmuxImageDisabledReason,
    },
}

pub fn parse_tmux_kitty_terminal_override(raw: Option<&str>) -> TmuxKittyTerminalOverride {
    match raw {
        Some("kitty") => Some(TmuxKittyTerminal::Kitty),
        Some("ghostty") => Some(TmuxKittyTerminal::Ghostty),
        Some("warp") => Some(TmuxKittyTerminal::Warp),
        _ => None,
    }
}

fn identity_for_terminal(terminal: TmuxKittyTerminal) -> (TmuxKittyPlacement, TmuxKittyTerminal) {
    let placement = if terminal == TmuxKittyTerminal::Warp {
        TmuxKittyPlacement::Direct
    } else {
        TmuxKittyPlacement::Placeholder
    };
    (placement, terminal)
}

fn identify_tmux_kitty_client(
    client_termname: &str,
    override_terminal: TmuxKittyTerminalOverride,
) -> Option<(TmuxKittyPlacement, TmuxKittyTerminal)> {
    if let Some(terminal) = override_terminal {
        return Some(identity_for_terminal(terminal));
    }
    let term = client_termname.trim().to_lowercase();
    if term.contains("kitty") {
        return Some(identity_for_terminal(TmuxKittyTerminal::Kitty));
    }
    if term.contains("ghostty") {
        return Some(identity_for_terminal(TmuxKittyTerminal::Ghostty));
    }
    if term.contains("warp") {
        return Some(identity_for_terminal(TmuxKittyTerminal::Warp));
    }
    None
}

fn topology_reason(base: &TmuxBaseState) -> Option<TmuxImageDisabledReason> {
    if base.support_tier == TmuxSupportTier::Unsupported {
        return Some(TmuxImageDisabledReason::UnsupportedVersion);
    }
    if base.nested {
        return Some(TmuxImageDisabledReason::Nested);
    }
    if base.client_count != 1 {
        return Some(TmuxImageDisabledReason::MultipleClients);
    }
    if !base.focus_events {
        return Some(TmuxImageDisabledReason::FocusEventsOff);
    }
    if base.allow_passthrough == TmuxAllowPassthrough::Off {
        return Some(TmuxImageDisabledReason::PassthroughOff);
    }
    None
}

pub fn can_track_tmux_image_focus(
    state: &TmuxImageState,
    override_terminal: TmuxKittyTerminalOverride,
) -> bool {
    let TmuxImageState::Detected { base, .. } = state else {
        return false;
    };
    if topology_reason(base).is_some() {
        return false;
    }
    let Some((placement, _)) = identify_tmux_kitty_client(&base.client_termname, override_terminal)
    else {
        return false;
    };
    placement != TmuxKittyPlacement::Direct || base.allow_passthrough == TmuxAllowPassthrough::On
}

pub fn decide_tmux_image_capability(
    state: &TmuxImageState,
    override_terminal: TmuxKittyTerminalOverride,
) -> TmuxImageDecision {
    let base = match state {
        TmuxImageState::Outside(_) => {
            return TmuxImageDecision::Disabled {
                reason: TmuxImageDisabledReason::OutsideTmux,
            }
        }
        TmuxImageState::Unavailable { .. } => {
            return TmuxImageDecision::Disabled {
                reason: TmuxImageDisabledReason::ProbeUnavailable,
            }
        }
        TmuxImageState::Detected { base, .. } => base,
    };

    if let Some(reason) = topology_reason(base) {
        return TmuxImageDecision::Disabled { reason };
    }
    if !base.visible {
        return TmuxImageDecision::Disabled {
            reason: TmuxImageDisabledReason::Hidden,
        };
    }
    let Some((placement, terminal)) =
        identify_tmux_kitty_client(&base.client_termname, override_terminal)
    else {
        return TmuxImageDecision::Disabled {
            reason: TmuxImageDisabledReason::UnknownClient,
        };
    };
    if placement == TmuxKittyPlacement::Direct && base.allow_passthrough != TmuxAllowPassthrough::On
    {
        return TmuxImageDecision::Disabled {
            reason: TmuxImageDisabledReason::UnsafeDirectPlacement,
        };
    }
    TmuxImageDecision::Enabled {
        placement,
        terminal,
    }
}

#[cfg(test)]
#[path = "tmux_image_capability_tests.rs"]
mod tests;
