use super::*;
use crate::terminal_capabilities::CellDimensions;
use crate::tmux_image_probe::TmuxImageState;

fn ready_base() -> TmuxBaseState {
    TmuxBaseState {
        support_tier: TmuxSupportTier::OnAndAll,
        allow_passthrough: TmuxAllowPassthrough::On,
        focus_events: true,
        pane_active: true,
        window_active: true,
        visible: true,
        client_count: 1,
        client_termname: "xterm-ghostty".to_string(),
        nested: false,
        hyperlinks: true,
        cell_dimensions: Some(CellDimensions {
            width_px: 9,
            height_px: 18,
        }),
    }
}

fn ready_state() -> TmuxImageState {
    TmuxImageState::Detected {
        base: ready_base(),
        version: "3.7b".to_string(),
    }
}

fn with_base(base: TmuxBaseState) -> TmuxImageState {
    TmuxImageState::Detected {
        base,
        version: "3.7b".to_string(),
    }
}

fn disabled(reason: TmuxImageDisabledReason) -> TmuxImageDecision {
    TmuxImageDecision::Disabled { reason }
}

#[test]
fn enables_only_safe_live_placeholder_clients() {
    let on = decide_tmux_image_capability(&ready_state(), None);
    let all = decide_tmux_image_capability(
        &with_base(TmuxBaseState {
            allow_passthrough: TmuxAllowPassthrough::All,
            ..ready_base()
        }),
        None,
    );

    assert_eq!(
        on,
        TmuxImageDecision::Enabled {
            placement: TmuxKittyPlacement::Placeholder,
            terminal: TmuxKittyTerminal::Ghostty,
        }
    );
    assert_eq!(all, on);
}

#[test]
fn does_not_trust_generic_clients_or_stale_process_environment() {
    let decision = decide_tmux_image_capability(
        &with_base(TmuxBaseState {
            client_termname: "xterm-256color".to_string(),
            ..ready_base()
        }),
        None,
    );
    assert_eq!(decision, disabled(TmuxImageDisabledReason::UnknownClient));
}

#[test]
fn supports_warp_from_live_identity_or_explicit_override() {
    let live = decide_tmux_image_capability(
        &with_base(TmuxBaseState {
            client_termname: "xterm-warp".to_string(),
            ..ready_base()
        }),
        None,
    );
    let overridden = decide_tmux_image_capability(
        &with_base(TmuxBaseState {
            client_termname: "xterm-256color".to_string(),
            ..ready_base()
        }),
        Some(TmuxKittyTerminal::Warp),
    );

    let expected = TmuxImageDecision::Enabled {
        placement: TmuxKittyPlacement::Direct,
        terminal: TmuxKittyTerminal::Warp,
    };
    assert_eq!(live, expected);
    assert_eq!(overridden, expected);
}

#[test]
fn keeps_wezterm_direct_placement_disabled() {
    let decision = decide_tmux_image_capability(
        &with_base(TmuxBaseState {
            client_termname: "xterm-wezterm".to_string(),
            ..ready_base()
        }),
        None,
    );
    assert_eq!(decision, disabled(TmuxImageDisabledReason::UnknownClient));
}

#[test]
fn disables_unsafe_topology_and_visibility_states() {
    let cases = [
        (
            with_base(TmuxBaseState {
                nested: true,
                ..ready_base()
            }),
            TmuxImageDisabledReason::Nested,
        ),
        (
            with_base(TmuxBaseState {
                client_count: 2,
                visible: false,
                ..ready_base()
            }),
            TmuxImageDisabledReason::MultipleClients,
        ),
        (
            with_base(TmuxBaseState {
                visible: false,
                window_active: false,
                ..ready_base()
            }),
            TmuxImageDisabledReason::Hidden,
        ),
        (
            with_base(TmuxBaseState {
                focus_events: false,
                ..ready_base()
            }),
            TmuxImageDisabledReason::FocusEventsOff,
        ),
        (
            with_base(TmuxBaseState {
                support_tier: TmuxSupportTier::Unsupported,
                ..ready_base()
            }),
            TmuxImageDisabledReason::UnsupportedVersion,
        ),
        (
            with_base(TmuxBaseState {
                allow_passthrough: TmuxAllowPassthrough::Off,
                ..ready_base()
            }),
            TmuxImageDisabledReason::PassthroughOff,
        ),
    ];

    for (state, reason) in cases {
        assert_eq!(decide_tmux_image_capability(&state, None), disabled(reason));
    }
}

#[test]
fn rejects_direct_placement_when_passthrough_is_all() {
    let decision = decide_tmux_image_capability(
        &with_base(TmuxBaseState {
            allow_passthrough: TmuxAllowPassthrough::All,
            client_termname: "xterm-warp".to_string(),
            ..ready_base()
        }),
        None,
    );
    assert_eq!(
        decision,
        disabled(TmuxImageDisabledReason::UnsafeDirectPlacement)
    );
}

#[test]
fn rejects_outside_and_unavailable_states() {
    assert_eq!(
        decide_tmux_image_capability(&TmuxImageState::Outside(TmuxBaseState::default()), None),
        disabled(TmuxImageDisabledReason::OutsideTmux)
    );
    assert_eq!(
        decide_tmux_image_capability(
            &TmuxImageState::Unavailable {
                base: TmuxBaseState::default(),
                reason: crate::tmux_image_probe::TmuxUnavailableReason::ProbeFailed,
            },
            None
        ),
        disabled(TmuxImageDisabledReason::ProbeUnavailable)
    );
}

#[test]
fn parses_terminal_overrides() {
    assert_eq!(
        parse_tmux_kitty_terminal_override(Some("kitty")),
        Some(TmuxKittyTerminal::Kitty)
    );
    assert_eq!(
        parse_tmux_kitty_terminal_override(Some("ghostty")),
        Some(TmuxKittyTerminal::Ghostty)
    );
    assert_eq!(
        parse_tmux_kitty_terminal_override(Some("warp")),
        Some(TmuxKittyTerminal::Warp)
    );
    assert_eq!(parse_tmux_kitty_terminal_override(Some("wezterm")), None);
    assert_eq!(parse_tmux_kitty_terminal_override(Some("KITTY")), None);
    assert_eq!(parse_tmux_kitty_terminal_override(None), None);
}

#[test]
fn tracks_focus_only_for_safe_live_clients() {
    assert!(can_track_tmux_image_focus(&ready_state(), None));
    assert!(!can_track_tmux_image_focus(
        &with_base(TmuxBaseState {
            client_termname: "xterm-warp".to_string(),
            allow_passthrough: TmuxAllowPassthrough::All,
            ..ready_base()
        }),
        None
    ));
    assert!(can_track_tmux_image_focus(
        &with_base(TmuxBaseState {
            client_termname: "xterm-warp".to_string(),
            allow_passthrough: TmuxAllowPassthrough::On,
            ..ready_base()
        }),
        None
    ));
    assert!(can_track_tmux_image_focus(
        &with_base(TmuxBaseState {
            client_termname: "xterm-warp".to_string(),
            ..ready_base()
        }),
        Some(TmuxKittyTerminal::Kitty)
    ));
    assert!(!can_track_tmux_image_focus(
        &with_base(TmuxBaseState {
            nested: true,
            ..ready_base()
        }),
        None
    ));
    assert!(!can_track_tmux_image_focus(
        &TmuxImageState::Outside(TmuxBaseState::default()),
        None
    ));
    assert!(can_track_tmux_image_focus(
        &ready_state(),
        Some(TmuxKittyTerminal::Kitty)
    ));
}

#[test]
fn identifies_kitty_clients_from_trimmed_lowercase_termnames() {
    let kitty = decide_tmux_image_capability(
        &with_base(TmuxBaseState {
            client_termname: "  XTerm-Kitty  ".to_string(),
            ..ready_base()
        }),
        None,
    );
    assert_eq!(
        kitty,
        TmuxImageDecision::Enabled {
            placement: TmuxKittyPlacement::Placeholder,
            terminal: TmuxKittyTerminal::Kitty,
        }
    );
}
