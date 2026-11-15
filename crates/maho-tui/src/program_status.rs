//! Port of senpi `packages/tui/src/program-status.ts`: the OSC 7501 Program Status Protocol
//! (rev 0.4), where a program tells its terminal whether it is idle, working, blocked on the
//! user, done or failed.

use std::sync::LazyLock;

use regex::Regex;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramStatusState {
    Idle,
    Working,
    Blocked,
    Done,
    Error,
}

impl ProgramStatusState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Working => "working",
            Self::Blocked => "blocked",
            Self::Done => "done",
            Self::Error => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramStatusKind {
    Permission,
    Question,
    Auth,
}

impl ProgramStatusKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Permission => "permission",
            Self::Question => "question",
            Self::Auth => "auth",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramStatusState,
    /// How the user must act; only sent with `blocked`.
    pub kind: Option<ProgramStatusKind>,
    /// One line on what the program is doing or waiting for.
    pub message: Option<String>,
}

const MAX_MESSAGE_BYTES: usize = 2048;
const APP_PATTERN: &str = r"^[A-Za-z0-9_.+-]{1,32}$";
// C0, DEL and C1 controls: a decoded msg holding any of them makes the terminal drop the whole report.
const CONTROL_PATTERN: &str = r"[\u{0}-\u{1f}\u{7f}-\u{9f}]+";
// Bidi overrides/isolates and invisible formatting, which a terminal may show outside the grid.
const INVISIBLE_FORMATTING_PATTERN: &str = r"[\u{ad}\u{61c}\u{180e}\u{200b}\u{200e}\u{200f}\u{202a}-\u{202e}\u{2060}-\u{2064}\u{2066}-\u{2069}\u{feff}]";

fn sanitize_message(message: &str) -> String {
    static INVISIBLE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(INVISIBLE_FORMATTING_PATTERN).expect("valid invisible formatting regex")
    });
    static CONTROL: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(CONTROL_PATTERN).expect("valid control regex"));
    static WHITESPACE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\s+").expect("valid whitespace regex"));
    let without_formatting = INVISIBLE.replace_all(message, "");
    let spaced = CONTROL.replace_all(&without_formatting, " ");
    WHITESPACE.replace_all(&spaced, " ").trim().to_string()
}

fn truncate_utf8(text: &str, max_bytes: usize) -> &str {
    if text.len() <= max_bytes {
        return text;
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Standard base64 with padding, matching Node's `Buffer.from(text, "utf8").toString("base64")`.
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut encoded = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = chunk.get(1).copied().unwrap_or(0) as u32;
        let b2 = chunk.get(2).copied().unwrap_or(0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        encoded.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        encoded.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        encoded.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        encoded.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    encoded
}

/// Build the OSC 7501 report for `status`, or the `state=clear` report that removes every record
/// when it is `None`. `app` is repeated in every report because each one replaces its record; an
/// app name outside the protocol's charset or length is left out rather than sent for the terminal
/// to discard.
pub fn encode_program_status(status: Option<&ProgramStatus>, app: Option<&str>) -> String {
    let Some(status) = status else {
        return "\x1b]7501;state=clear\x1b\\".to_string();
    };
    let state = status.state;
    let mut pairs = vec![format!("state={}", state.as_str())];
    if state == ProgramStatusState::Blocked
        && let Some(kind) = status.kind
    {
        pairs.push(format!("kind={}", kind.as_str()));
    }
    if let Some(app) = app.filter(|app| is_valid_app_name(app)) {
        pairs.push(format!("app={app}"));
    }
    let message = status
        .message
        .as_deref()
        .map(sanitize_message)
        .unwrap_or_default();
    if !message.is_empty() {
        pairs.push(format!(
            "msg={}",
            base64_encode(truncate_utf8(&message, MAX_MESSAGE_BYTES).as_bytes())
        ));
    }
    format!("\x1b]7501;{}\x1b\\", pairs.join(":"))
}

fn is_valid_app_name(app: &str) -> bool {
    static PATTERN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(APP_PATTERN).expect("valid app name regex"));
    PATTERN.is_match(app)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Protocol bytes are a retained contract: herdr-core and other OSC 7501 terminals parse
    /// exactly these reports.
    fn decoded_message(sequence: &str) -> Option<String> {
        let body = sequence
            .strip_prefix("\x1b]7501;")?
            .strip_suffix("\x1b\\")?;
        let encoded = body.split(':').find_map(|pair| pair.strip_prefix("msg="))?;
        Some(String::from_utf8(base64_decode(encoded)).expect("valid utf8"))
    }

    fn base64_decode(input: &str) -> Vec<u8> {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut bytes = Vec::new();
        let mut buffer = 0u32;
        let mut bits = 0;
        for byte in input.bytes().filter(|byte| *byte != b'=') {
            let value = ALPHABET
                .iter()
                .position(|candidate| *candidate == byte)
                .expect("base64 alphabet") as u32;
            buffer = (buffer << 6) | value;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                bytes.push((buffer >> bits) as u8);
            }
        }
        bytes
    }

    fn status(state: ProgramStatusState, message: Option<&str>) -> ProgramStatus {
        ProgramStatus {
            state,
            kind: None,
            message: message.map(str::to_string),
        }
    }

    #[test]
    fn encodes_each_state_with_the_app_repeated_and_the_message_as_base64() {
        assert_eq!(
            encode_program_status(Some(&status(ProgramStatusState::Idle, None)), Some("omo")),
            "\x1b]7501;state=idle:app=omo\x1b\\"
        );
        assert_eq!(
            encode_program_status(
                Some(&status(ProgramStatusState::Working, Some("Building docs"))),
                Some("make")
            ),
            "\x1b]7501;state=working:app=make:msg=QnVpbGRpbmcgZG9jcw==\x1b\\"
        );
        let blocked = ProgramStatus {
            state: ProgramStatusState::Blocked,
            kind: Some(ProgramStatusKind::Question),
            message: Some("Overwrite config.toml?".to_string()),
        };
        assert_eq!(
            encode_program_status(Some(&blocked), Some("make")),
            "\x1b]7501;state=blocked:kind=question:app=make:msg=T3ZlcndyaXRlIGNvbmZpZy50b21sPw==\x1b\\"
        );
        assert_eq!(
            encode_program_status(None, Some("omo")),
            "\x1b]7501;state=clear\x1b\\"
        );
    }

    #[test]
    fn sends_kind_only_with_blocked_and_leaves_out_an_app_the_protocol_would_reject() {
        let working_with_kind = ProgramStatus {
            state: ProgramStatusState::Working,
            kind: Some(ProgramStatusKind::Permission),
            message: None,
        };
        assert_eq!(
            encode_program_status(Some(&working_with_kind), Some("senpi")),
            "\x1b]7501;state=working:app=senpi\x1b\\"
        );
        let long = "a".repeat(33);
        for app in ["o mo", "omo:x", "", long.as_str()] {
            assert_eq!(
                encode_program_status(Some(&status(ProgramStatusState::Idle, None)), Some(app)),
                "\x1b]7501;state=idle\x1b\\",
                "app {app}"
            );
        }
        let longest = "a".repeat(32);
        assert_eq!(
            encode_program_status(
                Some(&status(ProgramStatusState::Idle, None)),
                Some(&longest)
            ),
            format!("\x1b]7501;state=idle:app={longest}\x1b\\")
        );
    }

    #[test]
    fn strips_controls_and_bidi_or_invisible_formatting_so_the_terminal_keeps_the_report() {
        let sequence = encode_program_status(
            Some(&status(
                ProgramStatusState::Blocked,
                Some("Auth\x1b]0;owned\x07 \u{202e}flow\u{2066}?\u{200b}\nnext\u{9b}\u{feff}"),
            )),
            Some("omo"),
        );
        assert_eq!(
            decoded_message(&sequence).as_deref(),
            Some("Auth ]0;owned flow? next")
        );
        assert_eq!(
            encode_program_status(
                Some(&status(ProgramStatusState::Idle, Some("\n\t\u{202e} "))),
                Some("omo")
            ),
            "\x1b]7501;state=idle:app=omo\x1b\\"
        );
    }

    #[test]
    fn truncates_the_decoded_message_to_2048_bytes_on_a_utf8_boundary() {
        let message = format!("a{}", "é".repeat(1024));
        let sequence = encode_program_status(
            Some(&status(ProgramStatusState::Working, Some(&message))),
            Some("omo"),
        );
        let decoded = decoded_message(&sequence).expect("message pair");
        assert_eq!(decoded, format!("a{}", "é".repeat(1023)));
        assert_eq!(decoded.len(), 2047);
        assert!(sequence.len() <= 4096);
    }
}
