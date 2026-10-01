use std::sync::{Arc, Mutex};

use super::*;
use crate::process_env::env_from;

fn exec_returning(output: &'static str) -> TmuxExecFile {
    Arc::new(move |_file: &str, _args: &[String]| Ok(output.to_string()))
}

type RecordedFileCalls = Arc<Mutex<Vec<(String, Vec<String>)>>>;
type RecordedArgLists = Arc<Mutex<Vec<Vec<String>>>>;

fn detected(state: &TmuxImageState) -> &TmuxBaseState {
    match state {
        TmuxImageState::Detected { base, .. } => base,
        other => panic!("expected detected state, got {other:?}"),
    }
}

#[test]
fn normalizes_historical_option_values_by_support_tier() {
    let cases = [
        ("3.2a", "", TmuxSupportTier::Unsupported, TmuxAllowPassthrough::Off),
        ("3.3", "1", TmuxSupportTier::OnOnly, TmuxAllowPassthrough::On),
        ("3.3a", "on", TmuxSupportTier::OnOnly, TmuxAllowPassthrough::On),
        ("3.4", "all", TmuxSupportTier::OnAndAll, TmuxAllowPassthrough::All),
        ("3.7b", "all", TmuxSupportTier::OnAndAll, TmuxAllowPassthrough::All),
    ];

    for (version, raw, tier, allow) in cases {
        assert_eq!(tmux_support_tier(version), tier, "tier for {version}");
        assert_eq!(
            normalize_tmux_allow_passthrough(raw, tier),
            allow,
            "allow for {version}/{raw}"
        );
    }
}

#[test]
fn uses_exec_file_compatible_argv_without_shell_quoting() {
    let calls: RecordedFileCalls = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&calls);
    let exec_file: TmuxExecFile = Arc::new(move |file: &str, args: &[String]| {
        recorder
            .lock()
            .unwrap()
            .push((file.to_string(), args.to_vec()));
        Ok("3.7b|on|on|1|1|1|xterm-ghostty|10|20|RGB,hyperlinks".to_string())
    });

    let env = env_from(&[
        ("TERM", "tmux-256color"),
        ("TMUX", "/tmp/tmux"),
        ("TMUX_PANE", "%42"),
    ]);
    let state = probe_tmux_image_state(&env, Some(&exec_file));

    let recorded = calls.lock().unwrap().clone();
    assert_eq!(
        recorded,
        vec![(
            "tmux".to_string(),
            vec![
                "display-message".to_string(),
                "-p".to_string(),
                "-t".to_string(),
                "%42".to_string(),
                TMUX_IMAGE_FORMAT.to_string(),
            ]
        )]
    );
    let base = detected(&state);
    assert!(base.visible);
    assert_eq!(base.client_count, 1);
    assert!(base.hyperlinks);
    assert_eq!(
        base.cell_dimensions,
        Some(CellDimensions {
            width_px: 10,
            height_px: 20
        })
    );
}

#[test]
fn marks_nested_and_multi_client_sessions_unsafe() {
    let env = env_from(&[("TMUX", "/tmp/tmux")]);
    let nested = probe_tmux_image_state(
        &env,
        Some(&exec_returning(
            "3.7b|on|on|1|1|1|tmux-256color|9|18|hyperlinks",
        )),
    );
    let multi_client = probe_tmux_image_state(
        &env,
        Some(&exec_returning(
            "3.7b|on|on|1|1|2|xterm-ghostty|9|18|hyperlinks",
        )),
    );

    assert!(detected(&nested).nested);
    assert!(!detected(&multi_client).visible);
}

#[test]
fn reports_outside_when_neither_tmux_nor_tmux_term() {
    let outside = probe_tmux_image_state(&env_from(&[("TERM", "xterm-256color")]), None);
    assert_eq!(outside, TmuxImageState::Outside(TmuxBaseState::default()));

    let empty_tmux = probe_tmux_image_state(&env_from(&[("TMUX", "")]), None);
    assert!(matches!(empty_tmux, TmuxImageState::Outside(_)));

    let by_term = probe_tmux_image_state(
        &env_from(&[("TERM", "tmux-256color")]),
        Some(&exec_returning("3.7b|on|on|1|1|1|xterm-ghostty|9|18|")),
    );
    assert!(matches!(by_term, TmuxImageState::Detected { .. }));
}

#[test]
fn reports_probe_failure_and_malformed_output() {
    let failing: TmuxExecFile = Arc::new(|_file: &str, _args: &[String]| Err("boom".to_string()));
    let env = env_from(&[("TMUX", "/tmp/tmux")]);
    assert_eq!(
        probe_tmux_image_state(&env, Some(&failing)),
        TmuxImageState::Unavailable {
            base: TmuxBaseState::default(),
            reason: TmuxUnavailableReason::ProbeFailed,
        }
    );
    assert_eq!(
        parse_tmux_image_state("3.7b|on"),
        TmuxImageState::Unavailable {
            base: TmuxBaseState::default(),
            reason: TmuxUnavailableReason::MalformedOutput,
        }
    );
}

#[test]
fn omits_cell_dimensions_unless_both_are_positive_integers() {
    let state = parse_tmux_image_state("3.7b|on|on|1|1|1|xterm-ghostty|0|18|");
    assert_eq!(detected(&state).cell_dimensions, None);

    let state = parse_tmux_image_state("3.7b|on|on|1|1|1|xterm-ghostty|9|abc|");
    assert_eq!(detected(&state).cell_dimensions, None);

    let state = parse_tmux_image_state("3.7b|on|on|1|1|1|xterm-ghostty|||");
    assert_eq!(detected(&state).cell_dimensions, None);
}

#[test]
fn ignores_a_tmux_pane_that_is_not_a_pane_id() {
    let calls: RecordedArgLists = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&calls);
    let exec_file: TmuxExecFile = Arc::new(move |_file: &str, args: &[String]| {
        recorder.lock().unwrap().push(args.to_vec());
        Ok("3.7b|on|on|1|1|1|xterm-ghostty|9|18|".to_string())
    });

    for pane in ["%", "42", "%-1", "%1x"] {
        let env = env_from(&[("TMUX", "/tmp/tmux"), ("TMUX_PANE", pane)]);
        probe_tmux_image_state(&env, Some(&exec_file));
    }

    for args in calls.lock().unwrap().iter() {
        assert_eq!(args.len(), 3, "pane id should be rejected: {args:?}");
    }

    let env = env_from(&[("TMUX", "/tmp/tmux"), ("TMUX_PANE", "%7")]);
    probe_tmux_image_state(&env, Some(&exec_file));
    let recorded = calls.lock().unwrap().clone();
    assert_eq!(recorded.last().map(Vec::len), Some(5));
}

#[test]
fn normalizes_allow_passthrough_and_versions_like_tmux() {
    assert_eq!(
        normalize_tmux_allow_passthrough(" 1 ", TmuxSupportTier::OnAndAll),
        TmuxAllowPassthrough::On
    );
    assert_eq!(
        normalize_tmux_allow_passthrough("ALL", TmuxSupportTier::OnOnly),
        TmuxAllowPassthrough::Off
    );
    assert_eq!(
        normalize_tmux_allow_passthrough("all", TmuxSupportTier::Unsupported),
        TmuxAllowPassthrough::Off
    );
    assert_eq!(
        normalize_tmux_allow_passthrough("0", TmuxSupportTier::OnAndAll),
        TmuxAllowPassthrough::Off
    );

    assert_eq!(tmux_support_tier("next-3.4"), TmuxSupportTier::OnAndAll);
    assert_eq!(tmux_support_tier(" 3.3 "), TmuxSupportTier::OnOnly);
    assert_eq!(tmux_support_tier("3.10"), TmuxSupportTier::OnAndAll);
    assert_eq!(tmux_support_tier("2.9"), TmuxSupportTier::Unsupported);
    assert_eq!(tmux_support_tier("next-"), TmuxSupportTier::Unsupported);
    assert_eq!(tmux_support_tier("3"), TmuxSupportTier::Unsupported);
    assert_eq!(tmux_support_tier(""), TmuxSupportTier::Unsupported);
}

#[test]
fn parses_boolean_and_nested_client_fields() {
    let state = parse_tmux_image_state("3.7b|on|ON|1|0|1|SCREEN-256color|9|18|");
    let base = detected(&state);
    assert!(base.focus_events);
    assert!(base.pane_active);
    assert!(!base.window_active);
    assert!(!base.visible);
    assert!(base.nested);

    let state = parse_tmux_image_state("3.7b|on|0|0|1|0|xterm-256color|9|18|");
    let base = detected(&state);
    assert!(!base.focus_events);
    assert_eq!(base.client_count, 0);
    assert!(!base.nested);
    assert!(!base.hyperlinks);
}
