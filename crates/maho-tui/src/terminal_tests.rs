//! Ports of senpi terminal.test.ts, terminal-detach.test.ts, terminal-cursor-position.test.ts,
//! terminal-external-stdout-guard.test.ts and terminal-sigwinch.test.ts.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::keys::is_kitty_protocol_active;
use crate::process_env::{env_from, with_overrides};

#[derive(Default)]
struct ManualClock(AtomicU64);

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }
}

impl ManualClock {
    fn advance(&self, ms: u64) {
        self.0.fetch_add(ms, Ordering::SeqCst);
    }
}

type Writes = Rc<RefCell<Vec<String>>>;
type RawModeBehavior = Box<dyn FnMut(bool) -> Result<(), TerminalError>>;

/// Scripted stdin/stdout: records writes, never touches the real tty.
struct ScriptedIo {
    writes: Writes,
    to_process_stdout: bool,
    size: Option<(u16, u16)>,
    raw_mode: Option<RawModeBehavior>,
}

impl ScriptedIo {
    fn new(writes: &Writes) -> Self {
        Self {
            writes: Rc::clone(writes),
            to_process_stdout: false,
            size: Some((80, 24)),
            raw_mode: None,
        }
    }
}

impl TerminalIo for ScriptedIo {
    fn write(&mut self, data: &str) {
        if self.to_process_stdout {
            process_stdio::stdout_write(data);
        } else {
            self.writes.borrow_mut().push(data.to_string());
        }
    }
    fn is_raw(&self) -> bool {
        false
    }
    fn set_raw_mode(&mut self, raw: bool) -> Result<(), TerminalError> {
        self.raw_mode.as_mut().map_or(Ok(()), |f| f(raw))
    }
    fn size(&self) -> Option<(u16, u16)> {
        self.size
    }
    fn read_input(&mut self, _timeout_ms: u64) -> Result<Option<Vec<u8>>, TerminalError> {
        Ok(None)
    }
    fn refresh_dimensions(&mut self) {}
}

/// Environment every harness starts from (the dev machine may run inside tmux or SSH).
const BASE_ENV: [(&str, Option<&str>); 9] = [
    ("PI_TUI_KEYBOARD_PROTOCOL", None),
    ("TMUX", None),
    ("TMUX_PANE", None),
    ("PI_TUI_WRITE_LOG", None),
    ("PI_TUI_ESC_TIMEOUT", None),
    ("SSH_CONNECTION", None),
    ("SSH_TTY", None),
    ("TERM_PROGRAM", None),
    ("COLUMNS", None),
];

fn with_env<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
    with_overrides(&BASE_ENV, || with_overrides(vars, f))
}

fn count(writes: &Writes, needle: &str) -> usize {
    writes.borrow().iter().filter(|w| *w == needle).count()
}

fn contains(writes: &Writes, needle: &str) -> bool {
    count(writes, needle) > 0
}

fn index_of(writes: &Writes, needle: &str) -> Option<usize> {
    writes.borrow().iter().position(|w| w == needle)
}

fn terminal_with(
    io: ScriptedIo,
    clock: &Arc<ManualClock>,
    tmux_exec_file: Option<TmuxExecFile>,
) -> ProcessTerminal {
    ProcessTerminal::new(ProcessTerminalOptions {
        io: Some(Box::new(io)),
        clock: Some(Arc::clone(clock) as Arc<dyn Clock>),
        tmux_exec_file,
        ..ProcessTerminalOptions::default()
    })
}

fn tick(terminal: &mut ProcessTerminal, clock: &ManualClock, ms: u64) {
    clock.advance(ms);
    terminal.run_due_timers();
}

fn err(message: &str) -> TerminalError {
    TerminalError::new(message)
}

// ---- terminal.test.ts: resolveEscapeTimeoutMs ----

#[test]
fn uses_pi_tui_esc_timeout_when_configured() {
    assert_eq!(
        resolve_escape_timeout_ms(&env_from(&[("PI_TUI_ESC_TIMEOUT", "80")])),
        80.0
    );
    assert_eq!(
        resolve_escape_timeout_ms(&env_from(&[
            ("PI_TUI_ESC_TIMEOUT", "80"),
            ("SSH_TTY", "/dev/pts/1")
        ])),
        80.0
    );
}

#[test]
fn ignores_invalid_pi_tui_esc_timeout_values() {
    for value in ["abc", "0", "-5", ""] {
        assert_eq!(
            resolve_escape_timeout_ms(&env_from(&[("PI_TUI_ESC_TIMEOUT", value)])),
            10.0,
            "{value:?}"
        );
    }
}

#[test]
fn defaults_to_100ms_over_ssh() {
    assert_eq!(
        resolve_escape_timeout_ms(&env_from(&[("SSH_CONNECTION", "10.0.0.1 22")])),
        100.0
    );
    assert_eq!(
        resolve_escape_timeout_ms(&env_from(&[("SSH_TTY", "/dev/pts/1")])),
        100.0
    );
}

#[test]
fn defaults_to_10ms_otherwise() {
    assert_eq!(resolve_escape_timeout_ms(&env_from(&[])), 10.0);
}

#[test]
fn escape_timeout_follows_js_number_coercion() {
    let t = |v: &str| resolve_escape_timeout_ms(&env_from(&[("PI_TUI_ESC_TIMEOUT", v)]));
    assert_eq!(t(" 25 "), 25.0);
    assert_eq!(t("0x20"), 32.0);
    assert_eq!(t("1e2"), 100.0);
    assert_eq!(t("12.5"), 12.5);
    assert_eq!(t("Infinity"), 10.0);
    assert_eq!(t("12px"), 10.0);
}

// ---- normalizeNativeShiftEnterInput / normalizeAppleTerminalInput ----

#[test]
fn rewrites_return_to_csi_u_shift_enter_when_native_shift_detection_is_enabled_and_shift_is_pressed()
 {
    assert_eq!(
        normalize_native_shift_enter_input("\r", true, true),
        "\x1b[13;2u"
    );
}

#[test]
fn leaves_return_unchanged_when_native_shift_detection_is_disabled() {
    assert_eq!(normalize_native_shift_enter_input("\r", false, true), "\r");
}

#[test]
fn leaves_return_unchanged_when_shift_is_not_pressed() {
    assert_eq!(normalize_native_shift_enter_input("\r", true, false), "\r");
}

#[test]
fn native_shift_enter_leaves_non_return_input_unchanged() {
    assert_eq!(
        normalize_native_shift_enter_input("\x1b[13;2u", true, true),
        "\x1b[13;2u"
    );
    assert_eq!(normalize_native_shift_enter_input("a", true, true), "a");
}

#[test]
fn rewrites_apple_terminal_return_to_csi_u_shift_enter_when_shift_is_pressed() {
    assert_eq!(
        normalize_apple_terminal_input("\r", true, true),
        "\x1b[13;2u"
    );
}

#[test]
fn leaves_apple_terminal_return_unchanged_when_shift_is_not_pressed() {
    assert_eq!(normalize_apple_terminal_input("\r", true, false), "\r");
}

#[test]
fn leaves_non_apple_terminal_return_unchanged_when_shift_is_pressed() {
    assert_eq!(normalize_apple_terminal_input("\r", false, true), "\r");
}

#[test]
fn apple_terminal_leaves_non_return_input_unchanged() {
    assert_eq!(
        normalize_apple_terminal_input("\x1b[13;2u", true, true),
        "\x1b[13;2u"
    );
    assert_eq!(normalize_apple_terminal_input("a", true, true), "a");
}

// ---- normalizeWarpWslShiftEnterInput ----

fn warp(
    data: &str,
    pairs: &[(&str, &str)],
    platform: &str,
    socket: Option<&dyn Fn(&str) -> bool>,
) -> String {
    normalize_warp_wsl_shift_enter_input(data, &env_from(pairs), platform, socket)
}

const VALID_WSL: [(&str, &str); 2] = [
    ("WARP_SESSION_ID", "session"),
    ("WSL_INTEROP", "/run/WSL/123_interop"),
];

#[test]
fn does_not_check_the_wsl_socket_when_warp_is_not_detected() {
    let checks = AtomicUsize::new(0);
    let check = |_: &str| {
        checks.fetch_add(1, Ordering::SeqCst);
        true
    };
    assert_eq!(
        warp(
            "\n",
            &[("WSL_INTEROP", "/run/WSL/321_interop")],
            "linux",
            Some(&check)
        ),
        "\n"
    );
    assert_eq!(checks.load(Ordering::SeqCst), 0);
}

#[test]
fn rejects_regular_files_directories_and_symlinks_to_non_sockets() {
    let dir = tempfile::tempdir().expect("tempdir");
    let regular = dir.path().join("regular-file");
    let directory = dir.path().join("directory");
    let symlink = dir.path().join("symlink-to-file");
    std::fs::write(&regular, "not a socket").expect("write");
    std::fs::create_dir(&directory).expect("mkdir");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&regular, &symlink).expect("symlink");
    for entry in [&regular, &directory, &symlink] {
        let entry = entry.to_string_lossy().into_owned();
        let check = |_: &str| default_socket_exists(&entry);
        assert_eq!(
            warp("\n", &VALID_WSL, "linux", Some(&check)),
            "\n",
            "{entry}"
        );
    }
}

#[cfg(unix)]
#[test]
fn accepts_a_symlink_to_a_real_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("interop.sock");
    let alias = dir.path().join("interop-alias");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    std::os::unix::fs::symlink(&socket, &alias).expect("symlink");
    let alias = alias.to_string_lossy().into_owned();
    let check = |_: &str| default_socket_exists(&alias);
    assert_eq!(warp("\n", &VALID_WSL, "linux", Some(&check)), "\x1b[13;2u");
}

#[test]
fn rewrites_warp_on_wsl_lf_as_explicit_shift_enter() {
    let yes = |_: &str| true;
    assert_eq!(
        warp(
            "\n",
            &[
                ("WARP_SESSION_ID", "session"),
                ("WSL_DISTRO_NAME", "Ubuntu"),
                ("WSL_INTEROP", "/run/WSL/1_interop")
            ],
            "linux",
            Some(&yes)
        ),
        "\x1b[13;2u"
    );
    assert_eq!(
        warp(
            "\n",
            &[
                ("WARP_SESSION_ID", "session"),
                ("WSL_INTEROP", "/run/WSL/1_interop")
            ],
            "linux",
            Some(&yes)
        ),
        "\x1b[13;2u"
    );
}

#[test]
fn rejects_spoofed_warp_and_wsl_markers() {
    let yes = |_: &str| true;
    let no = |_: &str| false;
    assert_eq!(
        warp(
            "\n",
            &[("WARP_SESSION_ID", ""), VALID_WSL[1]],
            "linux",
            Some(&yes)
        ),
        "\n"
    );
    assert_eq!(
        warp(
            "\n",
            &[("TERM_PROGRAM", "WarpTerminal"), VALID_WSL[1]],
            "linux",
            Some(&yes)
        ),
        "\n"
    );
    for interop in [
        "/run/WSL/not-a-real-socket",
        "/run/WSL/123",
        "/run/WSL/123_interop-extra",
        "/tmp/123_interop",
    ] {
        assert_eq!(
            warp(
                "\n",
                &[VALID_WSL[0], ("WSL_INTEROP", interop)],
                "linux",
                Some(&yes)
            ),
            "\n",
            "{interop}"
        );
    }
    assert_eq!(warp("\n", &VALID_WSL, "linux", Some(&no)), "\n");
}

#[test]
fn does_not_treat_empty_or_non_linux_wsl_markers_as_a_target_session() {
    let yes = |_: &str| true;
    assert_eq!(
        warp(
            "\n",
            &[("TERM_PROGRAM", "WarpTerminal"), ("WSL_DISTRO_NAME", "")],
            "linux",
            None
        ),
        "\n"
    );
    assert_eq!(
        warp(
            "\n",
            &[
                ("TERM_PROGRAM", "WarpTerminal"),
                ("WSL_INTEROP", "/run/WSL/1_interop")
            ],
            "darwin",
            Some(&yes)
        ),
        "\n"
    );
    assert_eq!(
        warp(
            "\n",
            &[("TERM_PROGRAM", "WarpTerminal"), ("WSL_INTEROP", "spoofed")],
            "linux",
            None
        ),
        "\n"
    );
}

#[test]
fn leaves_other_terminals_remote_or_multiplexed_sessions_and_input_unchanged() {
    assert_eq!(
        warp("\n", &[("WSL_DISTRO_NAME", "Ubuntu")], "linux", None),
        "\n"
    );
    assert_eq!(
        warp("\n", &[("TERM_PROGRAM", "WarpTerminal")], "linux", None),
        "\n"
    );
    let base = [
        ("TERM_PROGRAM", "WarpTerminal"),
        ("WSL_DISTRO_NAME", "Ubuntu"),
    ];
    assert_eq!(warp("\r", &base, node_platform(), None), "\r");
    for key in [
        "TMUX",
        "TMUX_PANE",
        "STY",
        "ZELLIJ",
        "SSH_CONNECTION",
        "SSH_CLIENT",
        "SSH_TTY",
    ] {
        let pairs = [base[0], base[1], (key, "active")];
        assert_eq!(warp("\n", &pairs, node_platform(), None), "\n", "{key}");
    }
}

// ---- Kitty keyboard protocol negotiation ----

struct Negotiation {
    terminal: ProcessTerminal,
    writes: Writes,
    input: Rc<RefCell<Option<String>>>,
    clock: Arc<ManualClock>,
}

impl Negotiation {
    fn new() -> Self {
        let writes: Writes = Rc::default();
        let clock = Arc::new(ManualClock::default());
        let mut terminal = terminal_with(ScriptedIo::new(&writes), &clock, None);
        let input: Rc<RefCell<Option<String>>> = Rc::default();
        let sink = Rc::clone(&input);
        terminal.set_input_handler(Box::new(move |data| {
            *sink.borrow_mut() = Some(data.to_string())
        }));
        terminal.query_and_enable_kitty_protocol();
        Self {
            terminal,
            writes,
            input,
            clock,
        }
    }

    fn send<'a>(&mut self, data: impl Into<StdinInput<'a>>) {
        self.terminal.feed(data);
    }

    fn input(&self) -> Option<String> {
        self.input.borrow().clone()
    }

    fn advance(&mut self, ms: u64) {
        tick(&mut self.terminal, &self.clock, ms);
    }

    fn cleanup(&mut self) {
        self.terminal.stop().expect("stop");
        set_kitty_protocol_active(false);
    }
}

#[test]
fn queries_kitty_mode_before_enabling_modify_other_keys_fallback() {
    with_env(&[], || {
        let mut h = Negotiation::new();
        assert_eq!(h.writes.borrow()[0], "\x1b[>7u\x1b[?u\x1b[c");
        assert!(!contains(&h.writes, "\x1b[>4;2m"));
        assert!(!h.terminal.kitty_protocol_active());
        h.cleanup();
    });
}

#[test]
fn activates_kitty_mode_for_non_zero_negotiated_flags() {
    with_env(&[], || {
        let mut h = Negotiation::new();
        h.send("\x1b[?7u");
        assert_eq!(h.input(), None);
        assert!(h.terminal.kitty_protocol_active());
        assert!(is_kitty_protocol_active());
        assert!(!contains(&h.writes, "\x1b[>4;2m"));
        assert!(!contains(&h.writes, "\x1b[>4;0m"));
        h.cleanup();
        assert_eq!(count(&h.writes, "\x1b[<u"), 1);
        assert!(!contains(&h.writes, "\x1b[>4;0m"));
        assert!(!is_kitty_protocol_active());
    });
}

#[test]
fn falls_back_to_modify_other_keys_for_zero_kitty_flags() {
    with_env(&[], || {
        let mut h = Negotiation::new();
        h.send("\x1b[?0u");
        assert_eq!(h.input(), None);
        assert!(!h.terminal.kitty_protocol_active());
        assert_eq!(count(&h.writes, "\x1b[>4;2m"), 1);
        h.cleanup();
        assert_eq!(count(&h.writes, "\x1b[>4;0m"), 1);
    });
}

#[test]
fn falls_back_to_modify_other_keys_for_device_attributes_without_kitty_flags() {
    with_env(&[], || {
        let mut h = Negotiation::new();
        h.send("\x1b[?62;4;52c");
        assert_eq!(h.input(), None);
        assert!(!h.terminal.kitty_protocol_active());
        assert_eq!(count(&h.writes, "\x1b[>4;2m"), 1);
        h.cleanup();
    });
}

#[test]
fn forwards_normal_input_while_waiting_for_kitty_response() {
    with_env(&[], || {
        let mut h = Negotiation::new();
        h.send("a");
        assert_eq!(h.input().as_deref(), Some("a"));
        assert!(!h.terminal.kitty_protocol_active());
        h.cleanup();
    });
}

#[test]
fn forwards_warp_on_wsl_lf_unchanged_for_non_editor_consumers() {
    with_env(
        &[
            ("TERM_PROGRAM", Some("WarpTerminal")),
            ("WARP_SESSION_ID", None),
            ("WARP_TERMINAL_SESSION_UUID", None),
            ("WSL_DISTRO_NAME", Some("Ubuntu")),
            ("WSL_INTEROP", None),
        ],
        || {
            let mut h = Negotiation::new();
            h.send("\n");
            assert_eq!(h.input().as_deref(), Some("\n"));
            h.send("\r");
            assert_eq!(h.input().as_deref(), Some("\r"));
            h.cleanup();
        },
    );
}

#[test]
fn reassembles_split_multibyte_buffer_chunks_before_forwarding_input() {
    with_env(&[], || {
        let mut h = Negotiation::new();
        h.send(&[0xe4u8, 0xb8][..]);
        assert_eq!(h.input(), None);
        h.send(&[0xadu8][..]);
        assert_eq!(h.input().as_deref(), Some("中"));
        h.cleanup();
    });
}

#[test]
fn tracks_split_kitty_confirmation() {
    with_env(&[], || {
        let mut h = Negotiation::new();
        h.send("\x1b[?7");
        h.advance(10);
        assert_eq!(h.input(), None);
        h.send("u");
        assert!(h.terminal.kitty_protocol_active());
        assert!(!contains(&h.writes, "\x1b[>4;2m"));
        h.cleanup();
    });
}

#[test]
fn replays_buffered_csi_prefix_input_when_it_is_not_a_kitty_response() {
    with_env(&[], || {
        let mut h = Negotiation::new();
        h.send("\x1b[");
        h.advance(50); // StdinBuffer sequence timeout, not the lone-ESC timeout
        assert_eq!(h.input(), None);
        h.advance(150);
        assert_eq!(h.input().as_deref(), Some("\x1b["));
        h.cleanup();
    });
}

#[test]
fn requests_modify_other_keys_immediately_when_running_inside_tmux() {
    with_env(
        &[
            ("TMUX", Some("/tmp/tmux-501/default,123,0")),
            ("TMUX_PANE", Some("%1")),
        ],
        || {
            let mut h = Negotiation::new();
            let modify = index_of(&h.writes, "\x1b[>4;2m").expect("modifyOtherKeys");
            let query = index_of(&h.writes, "\x1b[>7u\x1b[?u\x1b[c").expect("query");
            assert!(modify < query);
            h.send("\x1b[?7u");
            assert!(h.terminal.kitty_protocol_active());
            assert_eq!(count(&h.writes, "\x1b[>4;0m"), 1);
            h.cleanup();
        },
    );
}

#[test]
fn skips_enhanced_keyboard_protocols_when_disabled_while_preserving_input_delivery() {
    with_env(
        &[
            ("PI_TUI_KEYBOARD_PROTOCOL", Some("0")),
            ("TMUX", Some("/tmp/tmux-501/default,123,0")),
        ],
        || {
            let mut h = Negotiation::new();
            assert!(!keyboard_enhancement_enabled());
            assert!(!contains(&h.writes, "\x1b[>7u\x1b[?u\x1b[c"));
            assert!(!contains(&h.writes, "\x1b[>4;2m"));
            h.send("中");
            assert_eq!(h.input().as_deref(), Some("中"));
            h.cleanup();
            assert!(!contains(&h.writes, "\x1b[<u"));
            assert!(!contains(&h.writes, "\x1b[>4;0m"));
        },
    );
}

#[test]
fn keyboard_enhancement_switch_values() {
    for (value, expected) in [
        (None, true),
        (Some("1"), true),
        (Some("OFF"), false),
        (Some("No"), false),
        (Some("false"), false),
    ] {
        with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", value)], || {
            assert_eq!(keyboard_enhancement_enabled(), expected, "{value:?}");
        });
    }
}

// ---- stop ----

fn stop_harness(was_raw: bool, behavior: RawModeBehavior) -> ProcessTerminal {
    let writes: Writes = Rc::default();
    let mut io = ScriptedIo::new(&writes);
    io.raw_mode = Some(behavior);
    let clock = Arc::new(ManualClock::default());
    let mut terminal = terminal_with(io, &clock, None);
    terminal.set_was_raw(was_raw);
    terminal
}

#[test]
fn restores_the_previous_raw_mode_during_stop() {
    let modes = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&modes);
    let mut terminal = stop_harness(
        true,
        Box::new(move |mode| {
            sink.borrow_mut().push(mode);
            Ok(())
        }),
    );
    terminal.stop().expect("stop");
    assert_eq!(*modes.borrow(), vec![true]);
}

fn stop_with_raw_mode_error(error: &TerminalError) -> Result<(), TerminalError> {
    let thrown = error.clone();
    let mut terminal = stop_harness(false, Box::new(move |_| Err(thrown.clone())));
    terminal.stop()
}

#[test]
fn does_not_throw_when_raw_mode_restoration_fails_during_stop() {
    assert_eq!(
        stop_with_raw_mode_error(&err("setRawMode failed with errno: 5").with_errno(5)),
        Ok(())
    );
}

#[test]
fn does_not_throw_when_bun_reports_the_dead_terminal_only_in_the_message() {
    assert_eq!(
        stop_with_raw_mode_error(&err("setRawMode failed with errno: 5")),
        Ok(())
    );
}

#[test]
fn does_not_throw_when_the_message_carries_the_epipe_errno() {
    assert_eq!(
        stop_with_raw_mode_error(&err("setRawMode failed with errno: 32")),
        Ok(())
    );
}

#[test]
fn does_not_throw_when_node_reports_the_dead_terminal_by_error_code() {
    assert_eq!(
        stop_with_raw_mode_error(&err("setRawMode failed").with_code("EIO")),
        Ok(())
    );
}

#[test]
fn does_not_throw_for_the_other_dead_terminal_error_codes() {
    for error in [err("x").with_code("EPIPE"), err("x").with_code("ENOTCONN")] {
        assert_eq!(stop_with_raw_mode_error(&error), Ok(()), "{error:?}");
    }
}

#[test]
fn rethrows_raw_mode_failures_whose_message_carries_no_dead_terminal_errno() {
    let error = err("boom");
    assert_eq!(stop_with_raw_mode_error(&error), Err(error));
}

#[test]
fn rethrows_raw_mode_failures_carrying_a_non_dead_errno_in_the_message() {
    let error = err("setRawMode failed with errno: 22");
    assert_eq!(stop_with_raw_mode_error(&error), Err(error));
}

#[test]
fn rethrows_unexpected_raw_mode_restoration_failures() {
    let error = err("unexpected setRawMode failure").with_code("EINVAL");
    assert_eq!(stop_with_raw_mode_error(&error), Err(error));
}

// ---- progress, dimensions, title ----

#[test]
fn writes_a_valid_osc_9_4_clear_sequence() {
    let writes: Writes = Rc::default();
    let clock = Arc::new(ManualClock::default());
    let mut terminal = terminal_with(ScriptedIo::new(&writes), &clock, None);
    terminal.set_progress(false);
    assert_eq!(*writes.borrow(), vec!["\x1b]9;4;0\x07".to_string()]);
}

#[test]
fn progress_keepalive_repeats_every_second_until_cleared() {
    let writes: Writes = Rc::default();
    let clock = Arc::new(ManualClock::default());
    let mut terminal = terminal_with(ScriptedIo::new(&writes), &clock, None);
    terminal.set_progress(true);
    terminal.set_progress(true);
    tick(&mut terminal, &clock, 999);
    assert_eq!(count(&writes, "\x1b]9;4;3\x07"), 2);
    tick(&mut terminal, &clock, 1);
    assert_eq!(count(&writes, "\x1b]9;4;3\x07"), 3);
    terminal.set_progress(false);
    tick(&mut terminal, &clock, 5000);
    assert_eq!(count(&writes, "\x1b]9;4;3\x07"), 3);
}

#[test]
fn falls_back_to_columns_and_lines_before_default_dimensions() {
    let writes: Writes = Rc::default();
    let clock = Arc::new(ManualClock::default());
    let mut io = ScriptedIo::new(&writes);
    io.size = None;
    let terminal = terminal_with(io, &clock, None);
    with_env(&[("COLUMNS", Some("123")), ("LINES", Some("45"))], || {
        assert_eq!(terminal.columns(), 123);
        assert_eq!(terminal.rows(), 45);
    });
    with_env(&[("COLUMNS", Some("abc")), ("LINES", None)], || {
        assert_eq!(terminal.columns(), 80);
        assert_eq!(terminal.rows(), 24);
    });
}

fn capture_set_title(title: &str) -> Vec<String> {
    let writes: Writes = Rc::default();
    let clock = Arc::new(ManualClock::default());
    terminal_with(ScriptedIo::new(&writes), &clock, None).set_title(title);
    writes.borrow().clone()
}

#[test]
fn strips_control_characters_so_titles_cannot_escape_the_osc_sequence() {
    assert_eq!(
        capture_set_title("evil\x07\x1b[2Jrest\ntitle\u{9c}!"),
        vec!["\x1b]0;evil[2Jresttitle!\x07"]
    );
}

#[test]
fn passes_plain_titles_through_unchanged() {
    assert_eq!(
        capture_set_title("pi - my-project"),
        vec!["\x1b]0;pi - my-project\x07"]
    );
}

#[test]
fn move_by_and_clear_sequences() {
    let writes: Writes = Rc::default();
    let clock = Arc::new(ManualClock::default());
    let mut terminal = terminal_with(ScriptedIo::new(&writes), &clock, None);
    terminal.move_by(3);
    terminal.move_by(-2);
    terminal.move_by(0);
    terminal.hide_cursor();
    terminal.show_cursor();
    terminal.clear_line();
    terminal.clear_from_cursor();
    terminal.clear_screen();
    assert_eq!(
        *writes.borrow(),
        [
            "\x1b[3B",
            "\x1b[2A",
            "\x1b[?25l",
            "\x1b[?25h",
            "\x1b[K",
            "\x1b[J",
            "\x1b[2J\x1b[H"
        ]
    );
}

#[test]
fn write_appends_to_the_write_log_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join("tui.log");
    let log_str = log.to_string_lossy().into_owned();
    with_env(&[("PI_TUI_WRITE_LOG", Some(&log_str))], || {
        let writes: Writes = Rc::default();
        let clock = Arc::new(ManualClock::default());
        let mut terminal = terminal_with(ScriptedIo::new(&writes), &clock, None);
        terminal.write("a");
        terminal.write("b");
    });
    assert_eq!(std::fs::read_to_string(&log).expect("log"), "ab");
    let dir_str = dir.path().to_string_lossy().into_owned();
    let path = with_env(
        &[("PI_TUI_WRITE_LOG", Some(&dir_str))],
        resolve_write_log_path,
    );
    let name = Path::new(&path)
        .file_name()
        .expect("name")
        .to_string_lossy()
        .into_owned();
    let pattern = Regex::new(r"^tui-\d{4}-\d{2}-\d{2}_\d{2}-\d{2}-\d{2}-\d+\.log$").expect("regex");
    assert!(pattern.is_match(&name), "{name}");
}

#[test]
fn parses_negotiation_sequences() {
    use KeyboardProtocolNegotiationSequence::{
        CursorPosition as Cpr, DeviceAttributes, KittyFlags,
    };
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?7u"),
        Some(KittyFlags { flags: 7 })
    );
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?c"),
        Some(DeviceAttributes)
    );
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[?3;4R"),
        Some(Cpr(CursorPosition {
            row: 3,
            column: 4,
            page: None
        }))
    );
    assert_eq!(
        parse_keyboard_protocol_negotiation_sequence("\x1b[7u"),
        None
    );
}

// ---- terminal-detach.test.ts ----

fn eio_string_code() -> TerminalError {
    err("EIO: i/o error, read").with_code("EIO").with_errno(-5)
}

fn eio_numeric_errno() -> TerminalError {
    err("read failed with errno: 5").with_errno(5)
}

fn ebadf() -> TerminalError {
    err("EBADF: bad file descriptor, read").with_code("EBADF")
}

fn started_terminal(clock: &Arc<ManualClock>) -> ProcessTerminal {
    let writes: Writes = Rc::default();
    let mut terminal = terminal_with(ScriptedIo::new(&writes), clock, None);
    terminal.start(Box::new(|_| {}), Box::new(|| {}));
    terminal
}

#[test]
fn swallows_a_stdin_eio_reported_with_nodes_string_code_while_running() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let clock = Arc::new(ManualClock::default());
        let mut terminal = started_terminal(&clock);
        assert!(stdin_error_dispatcher_installed_for_tests());
        assert_eq!(stdin_error_subscriber_count_for_tests(), 1);
        assert_eq!(dispatch_stdin_error(eio_string_code()), Ok(()));
        terminal.stop().expect("stop");
    });
}

#[test]
fn swallows_a_stdin_eio_reported_with_buns_numeric_errno() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let clock = Arc::new(ManualClock::default());
        let mut terminal = started_terminal(&clock);
        assert_eq!(dispatch_stdin_error(eio_numeric_errno()), Ok(()));
        terminal.stop().expect("stop");
    });
}

#[test]
fn keeps_the_event_emitter_contract_for_non_eio_stdin_errors() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let clock = Arc::new(ManualClock::default());
        let mut terminal = started_terminal(&clock);
        assert_eq!(dispatch_stdin_error(ebadf()), Err(ebadf()));
        terminal.stop().expect("stop");
    });
}

#[test]
fn keeps_swallowing_a_late_eio_during_the_stop_grace_window() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let clock = Arc::new(ManualClock::default());
        let mut terminal = started_terminal(&clock);
        terminal.stop().expect("stop");
        tick(&mut terminal, &clock, 249);
        assert_eq!(dispatch_stdin_error(eio_string_code()), Ok(()));
    });
}

#[test]
fn removes_the_guard_once_the_stop_grace_window_expires() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let clock = Arc::new(ManualClock::default());
        let mut terminal = started_terminal(&clock);
        terminal.stop().expect("stop");
        tick(&mut terminal, &clock, 250);
        assert_eq!(stdin_error_subscriber_count_for_tests(), 0);
        assert!(!stdin_error_dispatcher_installed_for_tests());
        assert_eq!(
            dispatch_stdin_error(eio_string_code()),
            Err(eio_string_code())
        );
    });
}

#[test]
fn restart_inside_the_grace_window_keeps_one_subscription() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let clock = Arc::new(ManualClock::default());
        let mut terminal = started_terminal(&clock);
        terminal.stop().expect("stop");
        terminal.start(Box::new(|_| {}), Box::new(|| {}));
        tick(&mut terminal, &clock, 1000);
        assert_eq!(stdin_error_subscriber_count_for_tests(), 1);
        terminal.stop().expect("stop");
    });
}

// ---- terminal-sigwinch.test.ts ----

#[test]
fn does_not_panic_when_kill_returns_eacces_or_eperm_for_self_signal() {
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Other,
    ] {
        refresh_terminal_dimensions_with("linux", 42, |_| Err(std::io::Error::from(kind)));
    }
}

#[test]
fn does_not_call_kill_on_win32() {
    let mut called = false;
    refresh_terminal_dimensions_with("win32", 42, |_| {
        called = true;
        Ok(())
    });
    assert!(!called);
}

#[test]
fn signals_this_process_on_posix() {
    let mut target = None;
    refresh_terminal_dimensions_with("linux", 42, |pid| {
        target = Some(pid);
        Ok(())
    });
    assert_eq!(target, Some(42));
}

// ---- terminal-cursor-position.test.ts ----

struct Scripted {
    terminal: ProcessTerminal,
    writes: Writes,
    input: Rc<RefCell<Vec<String>>>,
    clock: Arc<ManualClock>,
    response: Option<&'static str>,
}

impl Scripted {
    fn new(
        response: Option<&'static str>,
        negotiate: bool,
        tmux_exec_file: Option<TmuxExecFile>,
    ) -> Self {
        Self::with_clock(
            response,
            negotiate,
            tmux_exec_file,
            Arc::new(ManualClock::default()),
        )
    }

    fn with_clock(
        response: Option<&'static str>,
        negotiate: bool,
        tmux_exec_file: Option<TmuxExecFile>,
        clock: Arc<ManualClock>,
    ) -> Self {
        let writes: Writes = Rc::default();
        let mut terminal = terminal_with(ScriptedIo::new(&writes), &clock, tmux_exec_file);
        let input: Rc<RefCell<Vec<String>>> = Rc::default();
        let sink = Rc::clone(&input);
        with_overrides(
            &[(
                "PI_TUI_KEYBOARD_PROTOCOL",
                Some(if negotiate { "1" } else { "0" }),
            )],
            || {
                terminal.start(
                    Box::new(move |data| sink.borrow_mut().push(data.to_string())),
                    Box::new(|| {}),
                );
            },
        );
        Self {
            terminal,
            writes,
            input,
            clock,
            response,
        }
    }

    /// \`queryCursorPosition()\`; a scripted stdout answers each CPR request synchronously.
    fn query(&mut self) -> CursorQueryTicket {
        let before = count(&self.writes, "\x1b[?6n");
        let ticket = self.terminal.query_cursor_position().expect("ticket");
        if let Some(response) = self.response
            && count(&self.writes, "\x1b[?6n") > before
        {
            self.terminal.feed(response);
        }
        ticket
    }

    fn send(&mut self, data: &str) {
        self.terminal.feed(data);
    }

    fn tick(&mut self, ms: u64) {
        tick(&mut self.terminal, &self.clock, ms);
    }

    fn input(&self) -> Vec<String> {
        self.input.borrow().clone()
    }
}

impl Drop for Scripted {
    fn drop(&mut self) {
        let _ = self.terminal.stop();
    }
}

const fn pos(row: u64, column: u64, page: Option<u64>) -> CursorPosition {
    CursorPosition { row, column, page }
}

fn tmux_exec(
    f: impl Fn(&str, &[String]) -> Result<String, String> + Send + Sync + 'static,
) -> Option<TmuxExecFile> {
    Some(Arc::new(f))
}

#[test]
fn uses_two_matching_tmux_readings_separated_by_at_least_10ms() {
    with_env(&[("TMUX_PANE", Some("%42"))], || {
        let clock = Arc::new(ManualClock::default());
        let calls = Arc::new(Mutex::new(Vec::<u64>::new()));
        let (calls_in, clock_in) = (Arc::clone(&calls), Arc::clone(&clock));
        let exec = tmux_exec(move |file, args| {
            assert_eq!(file, "tmux");
            assert_eq!(
                args,
                [
                    "display-message",
                    "-p",
                    "-t",
                    "%42",
                    "#{cursor_y} #{cursor_x}"
                ]
            );
            calls_in.lock().unwrap().push(clock_in.now_ms());
            Ok("11 4\n".to_string())
        });
        let mut s = Scripted::with_clock(None, false, exec, clock);
        let query = s.query();
        assert!(
            s.terminal
                .query_cursor_position()
                .expect("ticket")
                .is_same(&query)
        );
        assert_eq!(calls.lock().unwrap().len(), 1);
        s.tick(9);
        assert_eq!(calls.lock().unwrap().len(), 1);
        s.tick(1);
        assert_eq!(query.result(), Some(Some(pos(12, 5, None))));
        assert_eq!(*calls.lock().unwrap(), vec![0, 10]);
        assert!(contains(&s.writes, "\x1b[?6n"));
    });
}

#[test]
fn fails_closed_for_mismatched_or_malformed_tmux_output() {
    for output in [
        "12 4",
        "bad",
        "-1 4",
        "1.5 4",
        "1 2 3",
        "9007199254740991 0",
    ] {
        with_env(&[("TMUX_PANE", Some("%42"))], || {
            let calls = Arc::new(AtomicUsize::new(0));
            let counter = Arc::clone(&calls);
            let exec = tmux_exec(move |_, _| {
                let n = counter.fetch_add(1, Ordering::SeqCst) + 1;
                Ok(if n == 1 { "11 4" } else { output }.to_string())
            });
            let mut s = Scripted::new(None, false, exec);
            let query = s.query();
            s.tick(10);
            s.tick(740);
            assert_eq!(query.result(), Some(None), "{output}");
            assert_eq!(calls.load(Ordering::SeqCst), 2, "{output}");
        });
    }
}

#[test]
fn fails_closed_on_tmux_exec_error_without_accepting_private_replies() {
    with_env(&[("TMUX_PANE", Some("%42"))], || {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let exec = tmux_exec(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Err("exec failed".to_string())
        });
        let mut s = Scripted::new(None, false, exec);
        let query = s.query();
        s.send("\x1b[?12;5R");
        s.tick(750);
        assert_eq!(query.result(), Some(None));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(s.input().is_empty());
    });
}

#[test]
fn preserves_the_total_timeout_and_restart_only_recovery_for_tmux() {
    with_env(&[("TMUX_PANE", Some("%42"))], || {
        let clock = Arc::new(ManualClock::default());
        let calls = Arc::new(AtomicUsize::new(0));
        let (counter, clock_in) = (Arc::clone(&calls), Arc::clone(&clock));
        let exec = tmux_exec(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            clock_in.advance(750);
            Ok("11 4".to_string())
        });
        let mut s = Scripted::with_clock(None, false, exec, clock);
        let query = s.query();
        s.tick(750);
        assert_eq!(query.result(), Some(None));
        assert_eq!(s.query().result(), Some(None));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn never_executes_tmux_outside_a_pane_and_retains_private_query_bytes() {
    with_env(&[], || {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let exec = tmux_exec(move |_, _| {
            counter.fetch_add(1, Ordering::SeqCst);
            Ok("11 4".to_string())
        });
        let mut s = Scripted::new(Some("\x1b[?12;5R"), false, exec);
        assert_eq!(process_env::var("TMUX_PANE"), None);
        assert_eq!(s.query().result(), Some(Some(pos(12, 5, None))));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(count(&s.writes, "\x1b[?6n"), 1);
    });
}

#[test]
fn rejects_bare_cpr_shaped_function_keys() {
    assert_eq!(parse_cursor_position_response("\x1b[1;2R"), None);
}

#[test]
fn resolves_synchronous_private_replies() {
    for (reply, expected) in [
        ("\x1b[?12;1R", pos(12, 1, None)),
        ("\x1b[?12;1;1R", pos(12, 1, Some(1))),
    ] {
        with_env(&[], || {
            let mut s = Scripted::new(Some(reply), false, None);
            assert_eq!(s.query().result(), Some(Some(expected)));
            assert!(s.input().is_empty());
            assert_eq!(count(&s.writes, "\x1b[?6n"), 1);
        });
    }
}

#[test]
fn shares_one_pending_query_and_preserves_interleaved_keyboard_input() {
    with_env(&[], || {
        let mut s = Scripted::new(None, false, None);
        let first = s.query();
        assert!(s.query().is_same(&first));
        s.send("a");
        s.send("\x1b[?12;");
        s.send("1;1R");
        assert_eq!(first.result(), Some(Some(pos(12, 1, Some(1)))));
        assert_eq!(s.input(), ["a"]);
        s.send("\x1b[?12;1R");
        assert_eq!(s.input(), ["a"]);
    });
}

#[test]
fn forwards_bare_replies_and_times_out_instead_of_interpreting_function_keys() {
    with_env(&[], || {
        let mut s = Scripted::new(None, false, None);
        let query = s.query();
        s.send("\x1b[12;1R");
        s.tick(750);
        assert_eq!(query.result(), Some(None));
        assert_eq!(s.input(), ["\x1b[12;1R"]);
    });
}

#[test]
fn discards_late_private_replies_after_bounded_timeout() {
    with_env(&[], || {
        let mut s = Scripted::new(None, false, None);
        let query = s.query();
        s.tick(749);
        assert_eq!(query.result(), None);
        s.tick(1);
        assert_eq!(query.result(), Some(None));
        s.send("\x1b[?12;1R");
        assert!(s.input().is_empty());
        assert_eq!(
            s.query().result(),
            Some(None),
            "fail-closed until the next start"
        );
    });
}

#[test]
fn does_not_leak_a_late_fragmented_private_response_tail() {
    with_env(&[], || {
        let mut s = Scripted::new(None, false, None);
        let query = s.query();
        s.send("\x1b[?12;");
        s.tick(50);
        s.tick(150);
        s.tick(550);
        assert_eq!(query.result(), Some(None));
        s.send("1R");
        assert!(s.input().is_empty());
        s.send("a");
        assert_eq!(s.input(), ["a"]);
    });
}

#[test]
fn issues_cpr_only_after_keyboard_negotiation_settles() {
    with_env(&[], || {
        let mut s = Scripted::new(None, true, None);
        let query = s.query();
        assert!(!contains(&s.writes, "\x1b[?6n"));
        s.send("\x1b[?1u");
        assert!(contains(&s.writes, "\x1b[?6n"));
        s.send("\x1b[?12;1R");
        assert_eq!(query.result(), Some(Some(pos(12, 1, None))));
        set_kitty_protocol_active(false);
    });
}

#[test]
fn query_before_start_resolves_undefined() {
    let writes: Writes = Rc::default();
    let clock = Arc::new(ManualClock::default());
    let mut terminal = terminal_with(ScriptedIo::new(&writes), &clock, None);
    assert_eq!(
        terminal.query_cursor_position().expect("ticket").result(),
        Some(None)
    );
    assert!(writes.borrow().is_empty());
}

// ---- terminal-external-stdout-guard.test.ts ----

struct Guard {
    terminal: Option<ProcessTerminal>,
    writes: Arc<Mutex<Vec<String>>>,
    hidden: Arc<Mutex<Vec<String>>>,
    stub: StreamWrite,
    previous: Option<StreamWrite>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

enum GuardHandler {
    Record,
    Throw,
    None,
}

impl Guard {
    fn new(handler: GuardHandler) -> Self {
        let lock = process_stdio::TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let writes = Arc::new(Mutex::new(Vec::new()));
        let hidden = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&writes);
        let stub: StreamWrite = Arc::new(move |t: &str| sink.lock().unwrap().push(t.to_string()));
        let previous = process_stdio::set_stdout_writer(Arc::clone(&stub));
        let hidden_sink = Arc::clone(&hidden);
        let on_external_stdout_write: Option<ExternalStdoutHandler> = match handler {
            GuardHandler::Record => Some(Arc::new(move |t: &str| {
                hidden_sink.lock().unwrap().push(t.to_string());
                Ok(())
            })),
            GuardHandler::Throw => Some(Arc::new(|_: &str| Err("handler exploded".to_string()))),
            GuardHandler::None => None,
        };
        let local: Writes = Rc::default();
        let mut io = ScriptedIo::new(&local);
        io.to_process_stdout = true;
        let terminal = ProcessTerminal::new(ProcessTerminalOptions {
            io: Some(Box::new(io)),
            clock: Some(Arc::new(ManualClock::default())),
            on_external_stdout_write,
            ..ProcessTerminalOptions::default()
        });
        Self {
            terminal: Some(terminal),
            writes,
            hidden,
            stub,
            previous: Some(previous),
            _lock: lock,
        }
    }

    fn term(&mut self) -> &mut ProcessTerminal {
        self.terminal.as_mut().expect("terminal")
    }

    fn start(&mut self) {
        self.term().start(Box::new(|_| {}), Box::new(|| {}));
        self.writes.lock().unwrap().clear();
    }

    fn wrote(&self, needle: &str) -> bool {
        self.writes
            .lock()
            .unwrap()
            .iter()
            .any(|w| w.contains(needle))
    }

    fn hidden(&self) -> Vec<String> {
        self.hidden.lock().unwrap().clone()
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        if let Some(mut terminal) = self.terminal.take() {
            let _ = terminal.stop();
        }
        if let Some(previous) = self.previous.take() {
            process_stdio::set_stdout_writer(previous);
        }
    }
}

#[test]
fn hides_external_stdout_writes_while_started_and_forwards_them_to_the_handler() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let mut g = Guard::new(GuardHandler::Record);
        g.start();
        process_stdio::stdout_write("external junk\n");
        assert_eq!(g.hidden(), ["external junk\n"]);
        assert!(!g.wrote("external junk"));
    });
}

#[test]
fn lets_the_terminals_own_writes_reach_stdout_while_the_guard_is_active() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let mut g = Guard::new(GuardHandler::Record);
        g.start();
        g.term().write("\x1b[?2026hframe\x1b[?2026l");
        g.term().set_progress(true);
        g.term().set_progress(false);
        g.term().hide_cursor();
        assert!(g.wrote("frame"));
        assert!(g.wrote("\x1b]9;4;3\x07"));
        assert!(g.wrote("\x1b[?25l"));
        assert!(g.hidden().is_empty());
    });
}

#[test]
fn captures_console_log_while_started() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let mut g = Guard::new(GuardHandler::Record);
        g.start();
        process_stdio::console_log("stray library log");
        assert!(g.hidden().iter().any(|t| t.contains("stray library log")));
        assert!(!g.wrote("stray library log"));
    });
}

#[test]
fn restores_passthrough_after_stop() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let mut g = Guard::new(GuardHandler::Record);
        g.start();
        g.term().stop().expect("stop");
        g.writes.lock().unwrap().clear();
        process_stdio::stdout_write("after stop\n");
        assert!(g.wrote("after stop"));
        assert!(g.hidden().is_empty());
    });
}

#[test]
fn does_not_patch_stdout_when_no_handler_is_configured() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let mut g = Guard::new(GuardHandler::None);
        g.start();
        assert!(Arc::ptr_eq(&process_stdio::stdout_writer(), &g.stub));
        assert!(!g.term().is_stdout_guard_installed());
        process_stdio::stdout_write("plain passthrough\n");
        assert!(g.wrote("plain passthrough"));
    });
}

#[test]
fn falls_back_to_raw_stdout_when_the_handler_throws() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let mut g = Guard::new(GuardHandler::Throw);
        g.start();
        process_stdio::stdout_write("must not vanish\n");
        assert!(g.wrote("must not vanish"));
    });
}

#[test]
fn external_write_observers_see_passthrough_and_stderr_writes() {
    with_env(&[("PI_TUI_KEYBOARD_PROTOCOL", Some("0"))], || {
        let mut g = Guard::new(GuardHandler::None);
        let seen = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        g.start();
        let unsubscribe = g
            .term()
            .observe_external_writes(Arc::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
            }))
            .expect("observer");
        assert!(g.term().is_stdout_guard_installed());
        process_stdio::stdout_write("x");
        assert_eq!(seen.load(Ordering::SeqCst), 1);
        assert!(g.wrote("x"));
        g.term().hide_cursor();
        assert_eq!(
            seen.load(Ordering::SeqCst),
            1,
            "own writes are not external"
        );
        let stderr_sink: StreamWrite = Arc::new(|_: &str| {});
        let previous_stderr = process_stdio::set_stderr_writer(stderr_sink);
        g.term().stop().expect("stop");
        g.start();
        process_stdio::stderr_write("diag");
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        unsubscribe();
        process_stdio::stdout_write("y");
        assert_eq!(seen.load(Ordering::SeqCst), 2);
        g.term().stop().expect("stop");
        process_stdio::set_stderr_writer(previous_stderr);
    });
}
