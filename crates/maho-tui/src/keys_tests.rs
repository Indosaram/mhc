//! Port of senpi packages/tui/test/keys.test.ts (one #[test] per `it`). The Kitty protocol
//! flag is per-thread under cfg(test), so each test's set/reset stays local to it.

use super::*;
use crate::process_env::with_overrides;

#[test]
fn should_match_ctrl_c_when_pressing_ctrl_cyrillic_with_base_layout_key() {
    set_kitty_protocol_active(true);
    // Cyrillic 'с' = codepoint 1089, Latin 'c' = codepoint 99
    // Format: CSI 1089::99;5u (codepoint::base;modifier with ctrl=4, +1=5)
    let cyrillic_ctrl_c = "\x1b[1089::99;5u";
    assert!(matches_key(cyrillic_ctrl_c, "ctrl+c"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_match_ctrl_d_when_pressing_ctrl_cyrillic_with_base_layout_key() {
    set_kitty_protocol_active(true);
    // Cyrillic 'в' = codepoint 1074, Latin 'd' = codepoint 100
    let cyrillic_ctrl_d = "\x1b[1074::100;5u";
    assert!(matches_key(cyrillic_ctrl_d, "ctrl+d"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_match_ctrl_z_when_pressing_ctrl_cyrillic_with_base_layout_key() {
    set_kitty_protocol_active(true);
    // Cyrillic 'я' = codepoint 1103, Latin 'z' = codepoint 122
    let cyrillic_ctrl_z = "\x1b[1103::122;5u";
    assert!(matches_key(cyrillic_ctrl_z, "ctrl+z"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_match_ctrl_shift_p_with_base_layout_key() {
    set_kitty_protocol_active(true);
    // Cyrillic 'з' = codepoint 1079, Latin 'p' = codepoint 112
    // ctrl=4, shift=1, +1 = 6
    let cyrillic_ctrl_shift_p = "\x1b[1079::112;6u";
    assert!(matches_key(cyrillic_ctrl_shift_p, "ctrl+shift+p"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_still_match_direct_codepoint_when_no_base_layout_key() {
    set_kitty_protocol_active(true);
    // Latin ctrl+c without base layout key (terminal doesn't support flag 4)
    let latin_ctrl_c = "\x1b[99;5u";
    assert!(matches_key(latin_ctrl_c, "ctrl+c"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_match_super_modified_kitty_bindings_including_combined_modifiers() {
    set_kitty_protocol_active(true);
    assert!(matches_key("\x1b[107;9u", "super+k"));
    assert!(matches_key("\x1b[13;9u", "super+enter"));
    assert!(matches_key("\x1b[107;13u", &Key::ctrl_super("k")));
    assert!(matches_key("\x1b[107;13u", "ctrl+super+k"));
    assert!(matches_key("\x1b[107;14u", "ctrl+shift+super+k"));
    assert!(!matches_key("\x1b[107;13u", "super+k"));
    assert_eq!(parse_key("\x1b[107;9u").as_deref(), Some("super+k"));
    assert_eq!(parse_key("\x1b[13;9u").as_deref(), Some("super+enter"));
    assert_eq!(parse_key("\x1b[107;13u").as_deref(), Some("ctrl+super+k"));
    assert_eq!(
        parse_key("\x1b[107;14u").as_deref(),
        Some("shift+ctrl+super+k")
    );
    set_kitty_protocol_active(false);
}

#[test]
fn should_match_digit_bindings_via_kitty_csi_u() {
    set_kitty_protocol_active(true);
    assert!(matches_key("\x1b[49u", "1"));
    assert!(matches_key("\x1b[49;5u", "ctrl+1"));
    assert!(!matches_key("\x1b[49;5u", "ctrl+2"));
    assert_eq!(parse_key("\x1b[49u").as_deref(), Some("1"));
    assert_eq!(parse_key("\x1b[49;5u").as_deref(), Some("ctrl+1"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_match_modified_enter_bindings_via_kitty_csi_u() {
    let given_shift_enter = "\x1b[13;2u";
    let given_ctrl_enter = "\x1b[13;5u";
    let given_alt_enter = "\x1b[13;3u";
    set_kitty_protocol_active(false);
    let when_shift_enter_matches = matches_key(given_shift_enter, "shift+enter");
    let when_ctrl_enter_matches = matches_key(given_ctrl_enter, "ctrl+enter");
    let when_alt_enter_matches = matches_key(given_alt_enter, "alt+enter");
    let when_plain_enter_matches = matches_key(given_shift_enter, "enter");
    assert!(when_shift_enter_matches);
    assert!(when_ctrl_enter_matches);
    assert!(when_alt_enter_matches);
    assert!(!when_plain_enter_matches);
    assert_eq!(parse_key(given_shift_enter).as_deref(), Some("shift+enter"));
    assert_eq!(parse_key(given_ctrl_enter).as_deref(), Some("ctrl+enter"));
    assert_eq!(parse_key(given_alt_enter).as_deref(), Some("alt+enter"));
}

#[test]
fn should_normalize_kitty_keypad_functional_keys_to_logical_digits_symbols_and_navigation() {
    set_kitty_protocol_active(true);
    assert!(matches_key("\x1b[57400u", "1"));
    assert!(matches_key("\x1b[57410u", "/"));
    assert!(matches_key("\x1b[57417u", "left"));
    assert!(matches_key("\x1b[57426u", "delete"));
    assert_eq!(parse_key("\x1b[57399u").as_deref(), Some("0"));
    assert_eq!(parse_key("\x1b[57409u").as_deref(), Some("."));
    assert_eq!(parse_key("\x1b[57413u").as_deref(), Some("+"));
    assert_eq!(parse_key("\x1b[57416u").as_deref(), Some(","));
    assert_eq!(parse_key("\x1b[57417u").as_deref(), Some("left"));
    assert_eq!(parse_key("\x1b[57418u").as_deref(), Some("right"));
    assert_eq!(parse_key("\x1b[57419u").as_deref(), Some("up"));
    assert_eq!(parse_key("\x1b[57420u").as_deref(), Some("down"));
    assert_eq!(parse_key("\x1b[57421u").as_deref(), Some("pageUp"));
    assert_eq!(parse_key("\x1b[57422u").as_deref(), Some("pageDown"));
    assert_eq!(parse_key("\x1b[57423u").as_deref(), Some("home"));
    assert_eq!(parse_key("\x1b[57424u").as_deref(), Some("end"));
    assert_eq!(parse_key("\x1b[57425u").as_deref(), Some("insert"));
    assert_eq!(parse_key("\x1b[57426u").as_deref(), Some("delete"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_handle_shifted_key_in_format() {
    set_kitty_protocol_active(true);
    // Format with shifted key: CSI codepoint:shifted:base;modifier u
    // Latin 'c' with shifted 'C' (67) and base 'c' (99)
    let shifted_key = "\x1b[99:67:99;2u"; // shift modifier = 1, +1 = 2
    assert!(matches_key(shifted_key, "shift+c"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_handle_event_type_in_format() {
    set_kitty_protocol_active(true);
    // Format with event type: CSI codepoint::base;modifier:event u
    // Cyrillic ctrl+c release event (event type 3)
    let release_event = "\x1b[1089::99;5:3u";
    assert!(matches_key(release_event, "ctrl+c"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_handle_full_format_with_shifted_key_base_key_and_event_type() {
    set_kitty_protocol_active(true);
    // Full format: CSI codepoint:shifted:base;modifier:event u
    // Cyrillic 'С' (shifted) with base 'c', Ctrl+Shift pressed, repeat event
    // Cyrillic 'с' = 1089, Cyrillic 'С' = 1057, Latin 'c' = 99
    // ctrl=4, shift=1, +1 = 6, repeat event = 2
    let full_format = "\x1b[1089:1057:99;6:2u";
    assert!(matches_key(full_format, "ctrl+shift+c"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_prefer_codepoint_for_latin_letters_even_when_base_layout_differs() {
    set_kitty_protocol_active(true);
    // Dvorak Ctrl+K reports codepoint 'k' (107) and base layout 'v' (118)
    let dvorak_ctrl_k = "\x1b[107::118;5u";
    assert!(matches_key(dvorak_ctrl_k, "ctrl+k"));
    assert!(!matches_key(dvorak_ctrl_k, "ctrl+v"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_prefer_codepoint_for_symbol_keys_even_when_base_layout_differs() {
    set_kitty_protocol_active(true);
    // Dvorak Ctrl+/ reports codepoint '/' (47) and base layout '[' (91)
    let dvorak_ctrl_slash = "\x1b[47::91;5u";
    assert!(matches_key(dvorak_ctrl_slash, "ctrl+/"));
    assert!(!matches_key(dvorak_ctrl_slash, "ctrl+["));
    set_kitty_protocol_active(false);
}

#[test]
fn should_not_match_wrong_key_even_with_base_layout() {
    set_kitty_protocol_active(true);
    // Cyrillic ctrl+с with base 'c' should NOT match ctrl+d
    let cyrillic_ctrl_c = "\x1b[1089::99;5u";
    assert!(!matches_key(cyrillic_ctrl_c, "ctrl+d"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_not_match_wrong_modifiers_even_with_base_layout() {
    set_kitty_protocol_active(true);
    // Cyrillic ctrl+с should NOT match ctrl+shift+c
    let cyrillic_ctrl_c = "\x1b[1089::99;5u";
    assert!(!matches_key(cyrillic_ctrl_c, "ctrl+shift+c"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_match_xterm_modifyotherkeys_ctrl_c() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;5;99~", "ctrl+c"));
    assert_eq!(parse_key("\x1b[27;5;99~").as_deref(), Some("ctrl+c"));
}

#[test]
fn should_match_xterm_modifyotherkeys_ctrl_d() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;5;100~", "ctrl+d"));
    assert_eq!(parse_key("\x1b[27;5;100~").as_deref(), Some("ctrl+d"));
}

#[test]
fn should_match_xterm_modifyotherkeys_ctrl_z() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;5;122~", "ctrl+z"));
    assert_eq!(parse_key("\x1b[27;5;122~").as_deref(), Some("ctrl+z"));
}

#[test]
fn should_match_xterm_modifyotherkeys_enter_variants() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;5;13~", "ctrl+enter"));
    assert!(matches_key("\x1b[27;2;13~", "shift+enter"));
    assert!(matches_key("\x1b[27;3;13~", "alt+enter"));
    assert_eq!(parse_key("\x1b[27;5;13~").as_deref(), Some("ctrl+enter"));
    assert_eq!(parse_key("\x1b[27;2;13~").as_deref(), Some("shift+enter"));
    assert_eq!(parse_key("\x1b[27;3;13~").as_deref(), Some("alt+enter"));
}

#[test]
fn should_match_xterm_modifyotherkeys_tab_variants() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;2;9~", "shift+tab"));
    assert!(matches_key("\x1b[27;5;9~", "ctrl+tab"));
    assert!(matches_key("\x1b[27;3;9~", "alt+tab"));
    assert_eq!(parse_key("\x1b[27;2;9~").as_deref(), Some("shift+tab"));
    assert_eq!(parse_key("\x1b[27;5;9~").as_deref(), Some("ctrl+tab"));
    assert_eq!(parse_key("\x1b[27;3;9~").as_deref(), Some("alt+tab"));
}

#[test]
fn should_match_xterm_modifyotherkeys_backspace_variants() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;1;127~", "backspace"));
    assert!(matches_key("\x1b[27;5;127~", "ctrl+backspace"));
    assert!(matches_key("\x1b[27;3;127~", "alt+backspace"));
    assert_eq!(parse_key("\x1b[27;1;127~").as_deref(), Some("backspace"));
    assert_eq!(
        parse_key("\x1b[27;5;127~").as_deref(),
        Some("ctrl+backspace")
    );
    assert_eq!(
        parse_key("\x1b[27;3;127~").as_deref(),
        Some("alt+backspace")
    );
}

#[test]
fn should_match_xterm_modifyotherkeys_escape() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;1;27~", "escape"));
    assert_eq!(parse_key("\x1b[27;1;27~").as_deref(), Some("escape"));
}

#[test]
fn should_match_xterm_modifyotherkeys_space_variants() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;1;32~", "space"));
    assert!(matches_key("\x1b[27;5;32~", "ctrl+space"));
    assert_eq!(parse_key("\x1b[27;1;32~").as_deref(), Some("space"));
    assert_eq!(parse_key("\x1b[27;5;32~").as_deref(), Some("ctrl+space"));
}

#[test]
fn should_match_xterm_modifyotherkeys_symbol_combos() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;5;47~", "ctrl+/"));
    assert_eq!(parse_key("\x1b[27;5;47~").as_deref(), Some("ctrl+/"));
}

#[test]
fn should_match_xterm_modifyotherkeys_digit_combos() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;5;49~", "ctrl+1"));
    assert!(matches_key("\x1b[27;2;49~", "shift+1"));
    assert_eq!(parse_key("\x1b[27;5;49~").as_deref(), Some("ctrl+1"));
    assert_eq!(parse_key("\x1b[27;2;49~").as_deref(), Some("shift+1"));
}

#[test]
fn should_match_xterm_modifyotherkeys_shifted_uppercase_letters() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;2;69~", "shift+e"));
    assert!(matches_key("\x1b[27;6;69~", "ctrl+shift+e"));
    assert_eq!(parse_key("\x1b[27;2;69~").as_deref(), Some("shift+e"));
    assert_eq!(parse_key("\x1b[27;6;69~").as_deref(), Some("shift+ctrl+e"));
}

#[test]
fn should_match_ctrl_alt_letter_via_csi_u_when_kitty_inactive() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[104;7u", "ctrl+alt+h"));
    assert_eq!(parse_key("\x1b[104;7u").as_deref(), Some("ctrl+alt+h"));
}

#[test]
fn should_match_ctrl_alt_letter_via_xterm_modifyotherkeys() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b[27;7;104~", "ctrl+alt+h"));
    assert_eq!(parse_key("\x1b[27;7;104~").as_deref(), Some("ctrl+alt+h"));
}

#[test]
fn should_match_legacy_ctrl_c() {
    set_kitty_protocol_active(false);
    // Ctrl+c sends ASCII 3 (ETX)
    assert!(matches_key("\x03", "ctrl+c"));
}

#[test]
fn should_match_legacy_ctrl_d() {
    set_kitty_protocol_active(false);
    // Ctrl+d sends ASCII 4 (EOT)
    assert!(matches_key("\x04", "ctrl+d"));
}

#[test]
fn should_match_escape_key() {
    assert!(matches_key("\x1b", "escape"));
}

#[test]
fn should_match_legacy_linefeed_as_enter() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\n", "enter"));
    assert_eq!(parse_key("\n").as_deref(), Some("enter"));
}

#[test]
fn should_treat_linefeed_as_shift_enter_when_kitty_active() {
    set_kitty_protocol_active(true);
    assert!(matches_key("\n", "shift+enter"));
    assert!(!matches_key("\n", "enter"));
    assert_eq!(parse_key("\n").as_deref(), Some("shift+enter"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_parse_ctrl_space() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x00", "ctrl+space"));
    assert_eq!(parse_key("\x00").as_deref(), Some("ctrl+space"));
}

#[test]
fn should_match_legacy_ctrl_symbol() {
    set_kitty_protocol_active(false);
    // Ctrl+\ sends ASCII 28 (File Separator) in legacy terminals
    assert!(matches_key("\x1c", "ctrl+\\"));
    assert_eq!(parse_key("\x1c").as_deref(), Some("ctrl+\\"));
    // Ctrl+] sends ASCII 29 (Group Separator) in legacy terminals
    assert!(matches_key("\x1d", "ctrl+]"));
    assert_eq!(parse_key("\x1d").as_deref(), Some("ctrl+]"));
    // Ctrl+_ sends ASCII 31 (Unit Separator) in legacy terminals
    // Ctrl+- is on the same physical key on US keyboards
    assert!(matches_key("\x1f", "ctrl+_"));
    assert!(matches_key("\x1f", "ctrl+-"));
    assert_eq!(parse_key("\x1f").as_deref(), Some("ctrl+-"));
}

#[test]
fn should_match_legacy_ctrl_alt_symbol() {
    set_kitty_protocol_active(false);
    // Ctrl+Alt+[ sends ESC followed by ESC (Ctrl+[ = ESC)
    assert!(matches_key("\x1b\x1b", "ctrl+alt+["));
    assert_eq!(parse_key("\x1b\x1b").as_deref(), Some("ctrl+alt+["));
    // Ctrl+Alt+\ sends ESC followed by ASCII 28
    assert!(matches_key("\x1b\x1c", "ctrl+alt+\\"));
    assert_eq!(parse_key("\x1b\x1c").as_deref(), Some("ctrl+alt+\\"));
    // Ctrl+Alt+] sends ESC followed by ASCII 29
    assert!(matches_key("\x1b\x1d", "ctrl+alt+]"));
    assert_eq!(parse_key("\x1b\x1d").as_deref(), Some("ctrl+alt+]"));
    // Ctrl+_ sends ASCII 31 (Unit Separator) in legacy terminals
    // Ctrl+- is on the same physical key on US keyboards
    assert!(matches_key("\x1b\x1f", "ctrl+alt+_"));
    assert!(matches_key("\x1b\x1f", "ctrl+alt+-"));
    assert_eq!(parse_key("\x1b\x1f").as_deref(), Some("ctrl+alt+-"));
}

#[test]
fn should_treat_raw_0x08_as_plain_backspace_outside_windows_terminal() {
    set_kitty_protocol_active(false);
    with_overrides(&[("WT_SESSION", None)], || {
        assert!(matches_key("\x7f", "backspace"));
        assert!(!matches_key("\x7f", "ctrl+backspace"));
        assert_eq!(parse_key("\x7f").as_deref(), Some("backspace"));
        assert!(matches_key("\x08", "backspace"));
        assert!(!matches_key("\x08", "ctrl+backspace"));
        assert_eq!(parse_key("\x08").as_deref(), Some("backspace"));
        assert!(matches_key("\x08", "ctrl+h"));
    });
}

#[test]
fn should_treat_raw_0x08_as_ctrl_backspace_in_local_windows_terminal() {
    set_kitty_protocol_active(false);
    with_overrides(
        &[
            ("WT_SESSION", Some("test-session")),
            ("SSH_CONNECTION", None),
            ("SSH_CLIENT", None),
            ("SSH_TTY", None),
        ],
        || {
            assert!(matches_key("\x08", "ctrl+backspace"));
            assert!(!matches_key("\x08", "backspace"));
            assert_eq!(parse_key("\x08").as_deref(), Some("ctrl+backspace"));
            assert!(matches_key("\x08", "ctrl+h"));
        },
    );
}

#[test]
fn should_treat_raw_0x08_as_plain_backspace_in_windows_terminal_over_ssh() {
    set_kitty_protocol_active(false);
    with_overrides(
        &[
            ("WT_SESSION", Some("test-session")),
            ("SSH_CONNECTION", Some("1 2 3 4")),
            ("SSH_CLIENT", Some("1 2 3")),
            ("SSH_TTY", Some("/dev/pts/1")),
        ],
        || {
            assert!(!matches_key("\x08", "ctrl+backspace"));
            assert!(matches_key("\x08", "backspace"));
            assert_eq!(parse_key("\x08").as_deref(), Some("backspace"));
            assert!(matches_key("\x08", "ctrl+h"));
        },
    );
}

#[test]
fn should_parse_legacy_alt_prefixed_sequences_when_kitty_inactive() {
    set_kitty_protocol_active(false);
    assert!(matches_key("\x1b ", "alt+space"));
    assert_eq!(parse_key("\x1b ").as_deref(), Some("alt+space"));
    assert!(matches_key("\x1b\x08", "alt+backspace"));
    assert_eq!(parse_key("\x1b\x08").as_deref(), Some("alt+backspace"));
    assert!(matches_key("\x1b\x03", "ctrl+alt+c"));
    assert_eq!(parse_key("\x1b\x03").as_deref(), Some("ctrl+alt+c"));
    assert!(matches_key("\x1bB", "alt+left"));
    assert_eq!(parse_key("\x1bB").as_deref(), Some("alt+left"));
    assert!(matches_key("\x1bF", "alt+right"));
    assert_eq!(parse_key("\x1bF").as_deref(), Some("alt+right"));
    assert!(matches_key("\x1ba", "alt+a"));
    assert_eq!(parse_key("\x1ba").as_deref(), Some("alt+a"));
    assert!(matches_key("\x1b1", "alt+1"));
    assert_eq!(parse_key("\x1b1").as_deref(), Some("alt+1"));
    assert!(matches_key("\x1b,", "alt+,"));
    assert_eq!(parse_key("\x1b,").as_deref(), Some("alt+,"));
    assert!(matches_key("\x1b.", "alt+."));
    assert_eq!(parse_key("\x1b.").as_deref(), Some("alt+."));
    assert!(matches_key("\x1by", "alt+y"));
    assert_eq!(parse_key("\x1by").as_deref(), Some("alt+y"));
    assert!(matches_key("\x1bz", "alt+z"));
    assert_eq!(parse_key("\x1bz").as_deref(), Some("alt+z"));
    set_kitty_protocol_active(true);
    assert!(!matches_key("\x1b ", "alt+space"));
    assert_eq!(parse_key("\x1b "), None);
    assert!(matches_key("\x1b\x08", "alt+backspace"));
    assert_eq!(parse_key("\x1b\x08").as_deref(), Some("alt+backspace"));
    assert!(!matches_key("\x1b\x03", "ctrl+alt+c"));
    assert_eq!(parse_key("\x1b\x03"), None);
    assert!(!matches_key("\x1bB", "alt+left"));
    assert_eq!(parse_key("\x1bB"), None);
    assert!(!matches_key("\x1bF", "alt+right"));
    assert_eq!(parse_key("\x1bF"), None);
    assert!(!matches_key("\x1ba", "alt+a"));
    assert_eq!(parse_key("\x1ba"), None);
    assert!(!matches_key("\x1b1", "alt+1"));
    assert_eq!(parse_key("\x1b1"), None);
    assert!(!matches_key("\x1b,", "alt+,"));
    assert_eq!(parse_key("\x1b,"), None);
    assert!(!matches_key("\x1b.", "alt+."));
    assert_eq!(parse_key("\x1b."), None);
    assert!(!matches_key("\x1by", "alt+y"));
    assert_eq!(parse_key("\x1by"), None);
    set_kitty_protocol_active(false);
}

#[test]
fn should_match_arrow_keys() {
    assert!(matches_key("\x1b[A", "up"));
    assert!(matches_key("\x1b[B", "down"));
    assert!(matches_key("\x1b[C", "right"));
    assert!(matches_key("\x1b[D", "left"));
}

#[test]
fn should_match_ss3_arrows_and_home_end() {
    assert!(matches_key("\x1bOA", "up"));
    assert!(matches_key("\x1bOB", "down"));
    assert!(matches_key("\x1bOC", "right"));
    assert!(matches_key("\x1bOD", "left"));
    assert!(matches_key("\x1bOH", "home"));
    assert!(matches_key("\x1bOF", "end"));
}

#[test]
fn should_match_xterm_ctrl_modified_viewport_navigation() {
    assert!(matches_key("\x1b[1;5H", "ctrl+home"));
    assert!(matches_key("\x1b[1;5F", "ctrl+end"));
    assert!(matches_key("\x1b[5;5~", "ctrl+pageUp"));
    assert!(matches_key("\x1b[6;5~", "ctrl+pageDown"));
    assert_eq!(parse_key("\x1b[1;5H").as_deref(), Some("ctrl+home"));
    assert_eq!(parse_key("\x1b[1;5F").as_deref(), Some("ctrl+end"));
    assert_eq!(parse_key("\x1b[5;5~").as_deref(), Some("ctrl+pageUp"));
    assert_eq!(parse_key("\x1b[6;5~").as_deref(), Some("ctrl+pageDown"));
}

#[test]
fn should_match_legacy_function_keys_and_clear() {
    assert!(matches_key("\x1bOP", "f1"));
    assert!(matches_key("\x1b[24~", "f12"));
    assert!(matches_key("\x1b[E", "clear"));
}

#[test]
fn should_match_alt_arrows() {
    assert!(matches_key("\x1bp", "alt+up"));
    assert!(!matches_key("\x1bp", "up"));
}

#[test]
fn should_match_rxvt_modifier_sequences() {
    assert!(matches_key("\x1b[a", "shift+up"));
    assert!(matches_key("\x1bOa", "ctrl+up"));
    assert!(matches_key("\x1b[2$", "shift+insert"));
    assert!(matches_key("\x1b[2^", "ctrl+insert"));
    assert!(matches_key("\x1b[7$", "shift+home"));
}

#[test]
fn should_decode_kitty_keypad_functional_keys_to_printable_characters() {
    assert_eq!(decode_kitty_printable("\x1b[57399u").as_deref(), Some("0"));
    assert_eq!(decode_kitty_printable("\x1b[57400u").as_deref(), Some("1"));
    assert_eq!(decode_kitty_printable("\x1b[57409u").as_deref(), Some("."));
    assert_eq!(decode_kitty_printable("\x1b[57410u").as_deref(), Some("/"));
    assert_eq!(decode_kitty_printable("\x1b[57411u").as_deref(), Some("*"));
    assert_eq!(decode_kitty_printable("\x1b[57412u").as_deref(), Some("-"));
    assert_eq!(decode_kitty_printable("\x1b[57413u").as_deref(), Some("+"));
    assert_eq!(decode_kitty_printable("\x1b[57415u").as_deref(), Some("="));
    assert_eq!(decode_kitty_printable("\x1b[57416u").as_deref(), Some(","));
    assert_eq!(decode_kitty_printable("\x1b[57417u"), None);
}

#[test]
fn should_decode_printable_xterm_modifyotherkeys_sequences() {
    assert_eq!(decode_printable_key("\x1b[27;2;69~").as_deref(), Some("E"));
    assert_eq!(decode_printable_key("\x1b[27;2;196~").as_deref(), Some("Ä"));
    assert_eq!(decode_printable_key("\x1b[27;2;32~").as_deref(), Some(" "));
    assert_eq!(decode_printable_key("\x1b[27;2;13~"), None);
    assert_eq!(decode_printable_key("\x1b[27;6;69~"), None);
}

#[test]
fn should_return_latin_key_name_when_base_layout_key_is_present() {
    set_kitty_protocol_active(true);
    // Cyrillic ctrl+с with base layout 'c'
    let cyrillic_ctrl_c = "\x1b[1089::99;5u";
    assert_eq!(parse_key(cyrillic_ctrl_c).as_deref(), Some("ctrl+c"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_prefer_codepoint_for_latin_letters_when_base_layout_differs() {
    set_kitty_protocol_active(true);
    // Dvorak Ctrl+K reports codepoint 'k' (107) and base layout 'v' (118)
    let dvorak_ctrl_k = "\x1b[107::118;5u";
    assert_eq!(parse_key(dvorak_ctrl_k).as_deref(), Some("ctrl+k"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_prefer_codepoint_for_symbol_keys_when_base_layout_differs() {
    set_kitty_protocol_active(true);
    // Dvorak Ctrl+/ reports codepoint '/' (47) and base layout '[' (91)
    let dvorak_ctrl_slash = "\x1b[47::91;5u";
    assert_eq!(parse_key(dvorak_ctrl_slash).as_deref(), Some("ctrl+/"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_return_key_name_from_codepoint_when_no_base_layout() {
    set_kitty_protocol_active(true);
    let latin_ctrl_c = "\x1b[99;5u";
    assert_eq!(parse_key(latin_ctrl_c).as_deref(), Some("ctrl+c"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_parse_shifted_uppercase_csi_u_letters_as_shift_letter() {
    set_kitty_protocol_active(true);
    assert!(matches_key("\x1b[69;2u", "shift+e"));
    assert_eq!(parse_key("\x1b[69;2u").as_deref(), Some("shift+e"));
    set_kitty_protocol_active(false);
}

#[test]
fn should_ignore_kitty_csi_u_with_unsupported_modifiers() {
    set_kitty_protocol_active(true);
    assert_eq!(parse_key("\x1b[99;17u"), None);
    set_kitty_protocol_active(false);
}

#[test]
fn should_parse_legacy_ctrl_letter() {
    set_kitty_protocol_active(false);
    assert_eq!(parse_key("\x03").as_deref(), Some("ctrl+c"));
    assert_eq!(parse_key("\x04").as_deref(), Some("ctrl+d"));
}

#[test]
fn should_parse_special_keys() {
    assert_eq!(parse_key("\x1b").as_deref(), Some("escape"));
    assert_eq!(parse_key("\t").as_deref(), Some("tab"));
    assert_eq!(parse_key("\r").as_deref(), Some("enter"));
    assert_eq!(parse_key("\n").as_deref(), Some("enter"));
    assert_eq!(parse_key("\x00").as_deref(), Some("ctrl+space"));
    assert_eq!(parse_key(" ").as_deref(), Some("space"));
    assert_eq!(parse_key("1").as_deref(), Some("1"));
    assert!(matches_key("1", "1"));
}

#[test]
fn should_parse_arrow_keys() {
    assert_eq!(parse_key("\x1b[A").as_deref(), Some("up"));
    assert_eq!(parse_key("\x1b[B").as_deref(), Some("down"));
    assert_eq!(parse_key("\x1b[C").as_deref(), Some("right"));
    assert_eq!(parse_key("\x1b[D").as_deref(), Some("left"));
}

#[test]
fn should_parse_ss3_arrows_and_home_end() {
    assert_eq!(parse_key("\x1bOA").as_deref(), Some("up"));
    assert_eq!(parse_key("\x1bOB").as_deref(), Some("down"));
    assert_eq!(parse_key("\x1bOC").as_deref(), Some("right"));
    assert_eq!(parse_key("\x1bOD").as_deref(), Some("left"));
    assert_eq!(parse_key("\x1bOH").as_deref(), Some("home"));
    assert_eq!(parse_key("\x1bOF").as_deref(), Some("end"));
}

#[test]
fn should_parse_legacy_function_and_modifier_sequences() {
    assert_eq!(parse_key("\x1bOP").as_deref(), Some("f1"));
    assert_eq!(parse_key("\x1b[24~").as_deref(), Some("f12"));
    assert_eq!(parse_key("\x1b[E").as_deref(), Some("clear"));
    assert_eq!(parse_key("\x1b[2^").as_deref(), Some("ctrl+insert"));
    assert_eq!(parse_key("\x1bp").as_deref(), Some("alt+up"));
}

#[test]
fn should_parse_double_bracket_pageup() {
    assert_eq!(parse_key("\x1b[[5~").as_deref(), Some("pageUp"));
}
