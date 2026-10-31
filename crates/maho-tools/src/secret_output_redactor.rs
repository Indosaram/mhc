//! Streaming secret redactor for shell output (senpi `core/tools/secret-output-redactor.ts`).
//!
//! One redactor per output stream. `push` holds incomplete tokens; `finish` seals EOF.
//! `flush_idle` publishes ordinary fragments but retains their logical token for re-evaluation.

use base64::{
    Engine,
    alphabet::URL_SAFE,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use regex::Regex;
use std::sync::LazyLock;

const REDACTED: &str = "<redacted>";
const SECRET_PREFIXES: &str =
    "ghp_|gho_|ghr_|ghs_|ghu_|github_pat_|xoxa-|xoxb-|xoxp-|xoxo-|xoxr-|xoxs-|sk-ant-|sk-proj-|sk-";
/// Never release an undecided prefix to bound memory: an oversized token is redacted whole.
const MAX_TOKEN_CHARS: usize = 8192;

static SECRET_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|_)(?:KEY|TOKEN|SECRET|AUTH|AUTHORIZATION)$|PASSWORD")
        .expect("secret key regex")
});
static PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:^|[^A-Za-z0-9])(?:{SECRET_PREFIXES})[A-Za-z0-9_./+~=-]+"
    ))
    .expect("prefix regex")
});
static STARTED_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?:^|[^A-Za-z0-9])(?:{SECRET_PREFIXES}|AKIA|AIza|eyJ)"
    ))
    .expect("started prefix regex")
});
static CLOUD_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[^A-Za-z0-9])(?:AKIA[A-Z0-9]{16}|AIza[A-Za-z0-9_-]{35})")
        .expect("cloud key regex")
});
static JWT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^([A-Za-z0-9_-]+)\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]*\.?$").expect("jwt regex")
});
static PRIVATE_HEADER: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^-----BEGIN (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----$")
        .expect("pem header regex")
});
static PRIVATE_END: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"-----END (?:[A-Z0-9]+ )*PRIVATE KEY(?: BLOCK)?-----$").expect("pem end regex")
});

fn is_token_char(character: char) -> bool {
    matches!(character, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '.' | '/' | '+' | '~' | '=' | '-')
}

fn is_identifier(token: &str) -> bool {
    let mut chars = token.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn char_len(text: &str) -> usize {
    text.chars().count()
}

/// `^(?=.{8,}$)(?=.*[0-9_./+~=-])[A-Za-z0-9_./+~=-]+$`
fn is_bearer_value(token: &str) -> bool {
    char_len(token) >= 8
        && token.chars().all(is_token_char)
        && token
            .chars()
            .any(|c| matches!(c, '0'..='9' | '_' | '.' | '/' | '+' | '~' | '=' | '-'))
}

fn is_jwt(token: &str) -> bool {
    let Some(header) = JWT.captures(token).and_then(|captures| captures.get(1)) else {
        return false;
    };
    let engine = GeneralPurpose::new(
        &URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    );
    let Ok(bytes) = engine.decode(header.as_str()) else {
        return false;
    };
    matches!(
        serde_json::from_slice::<serde_json::Value>(&bytes),
        Ok(serde_json::Value::Object(map)) if map.get("alg").is_some_and(serde_json::Value::is_string)
    )
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Escape {
    Start,
    Charset,
    Csi,
    Osc,
    OscEnd,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ValueState {
    Start,
    Bare,
    Quoted,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Pem {
    Header,
    Body,
}

#[derive(Default)]
pub struct SecretOutputRedactor {
    token: String,
    raw_token: String,
    controls: String,
    escape: Option<Escape>,
    escape_text: String,
    oversized: bool,
    bearer: bool,
    value: Option<ValueState>,
    quote: char,
    escaped: bool,
    pem: Option<Pem>,
    pem_tail: String,
    finished: bool,
}

impl SecretOutputRedactor {
    pub fn new() -> Self {
        Self::default()
    }

    fn reset_token(&mut self) {
        self.token.clear();
        self.raw_token.clear();
        self.controls.clear();
    }

    pub fn push(&mut self, text: &str) -> String {
        if self.finished {
            return String::new();
        }
        let mut output = String::new();
        for character in text.chars() {
            if let Some(escape) = self.escape {
                if escape == Escape::Osc || escape == Escape::OscEnd {
                    if character == '\x07' || (escape == Escape::OscEnd && character == '\\') {
                        self.escape = None;
                    } else {
                        self.escape = Some(if character == '\x1b' {
                            Escape::OscEnd
                        } else {
                            Escape::Osc
                        });
                    }
                    continue;
                }
                self.escape_text.push(character);
                if escape == Escape::Start && character == ']' {
                    self.escape = Some(Escape::Osc);
                    self.escape_text.clear();
                    continue;
                }
                if escape == Escape::Start && character == '[' {
                    self.escape = Some(Escape::Csi);
                    continue;
                }
                if escape == Escape::Start && matches!(character, '(' | ')' | '*' | '+') {
                    self.escape = Some(Escape::Charset);
                    continue;
                }
                if escape == Escape::Start
                    || escape == Escape::Charset
                    || ('@'..='~').contains(&character)
                    || char_len(&self.escape_text) > 64
                {
                    if self.pem.is_none() && self.value.is_none() {
                        if self.oversized {
                            output.push_str(&self.escape_text);
                        } else {
                            self.raw_token.push_str(&self.escape_text);
                            self.controls.push_str(&self.escape_text);
                        }
                        if char_len(&self.raw_token) > MAX_TOKEN_CHARS {
                            self.reset_token();
                            self.oversized = true;
                            output.push_str(REDACTED);
                        }
                    }
                    self.escape = None;
                    self.escape_text.clear();
                }
                continue;
            }
            if character == '\x1b' {
                self.escape = Some(Escape::Start);
                self.escape_text = character.to_string();
                continue;
            }
            if let Some(pem) = self.pem {
                self.pem_tail.push(character);
                if pem == Pem::Header {
                    if self.pem_tail.ends_with("-----") {
                        if PRIVATE_HEADER.is_match(&self.pem_tail) {
                            output.push_str(REDACTED);
                            self.pem = Some(Pem::Body);
                        } else {
                            output.push_str(&self.pem_tail);
                            self.pem = None;
                        }
                        self.pem_tail.clear();
                    } else if char_len(&self.pem_tail) > 128 || character == '\n' {
                        output.push_str(&self.pem_tail);
                        self.pem_tail.clear();
                        self.pem = None;
                    }
                } else {
                    let length = char_len(&self.pem_tail);
                    if length > 128 {
                        self.pem_tail = self.pem_tail.chars().skip(length - 128).collect();
                    }
                    if PRIVATE_END.is_match(&self.pem_tail) {
                        self.pem_tail.clear();
                        self.pem = None;
                    }
                }
                continue;
            }
            if let Some(mut value) = self.value {
                if value == ValueState::Start {
                    if character == ' ' || character == '\t' {
                        output.push(character);
                        continue;
                    }
                    if character == '"' || character == '\'' {
                        self.quote = character;
                        self.value = Some(ValueState::Quoted);
                        output.push(character);
                        output.push_str(REDACTED);
                        continue;
                    }
                    value = ValueState::Bare;
                    self.value = Some(ValueState::Bare);
                    output.push_str(REDACTED);
                }
                if value == ValueState::Quoted {
                    if character == '\n' || character == '\r' {
                        self.value = None;
                        self.escaped = false;
                        output.push(character);
                    } else if self.escaped {
                        self.escaped = false;
                    } else if character == '\\' {
                        self.escaped = true;
                    } else if character == self.quote {
                        output.push(character);
                        self.value = None;
                    }
                    continue;
                }
                if !(character.is_whitespace() || character == '\'' || character == '"') {
                    continue;
                }
                self.value = None;
            }
            if character == '=' && !self.bearer && !self.oversized && is_identifier(&self.token) {
                let secret = SECRET_KEY.is_match(&self.token);
                output.push_str(&self.flush_token());
                output.push('=');
                if secret {
                    self.value = Some(ValueState::Start);
                }
                continue;
            }
            if is_token_char(character) {
                if self.oversized {
                    continue;
                }
                self.token.push(character);
                self.raw_token.push(character);
                if char_len(&self.raw_token) > MAX_TOKEN_CHARS
                    || char_len(&self.token) > MAX_TOKEN_CHARS
                {
                    self.reset_token();
                    self.oversized = true;
                    output.push_str(REDACTED);
                }
                continue;
            }
            if self.token == "-----BEGIN" && character == ' ' {
                self.pem = Some(Pem::Header);
                self.pem_tail = format!("{}{character}", self.token);
                self.reset_token();
                continue;
            }
            output.push_str(&self.flush_token());
            output.push(character);
            if !character.is_whitespace() {
                self.bearer = false;
            }
        }
        output
    }

    /// Publish ordinary prompts without treating the pause as a token boundary.
    /// Once a public credential prefix is present, withhold the token until it ends.
    pub fn flush_idle(&mut self) -> String {
        if self.finished || self.oversized {
            return String::new();
        }
        // A PEM opener has spaces: keep parsing it rather than suppressing only its
        // first word and accidentally releasing the following private-key body.
        if !self.token.is_empty() && "-----BEGIN".starts_with(&self.token) {
            return String::new();
        }
        if self.started_prefix().is_some() || self.is_secret(&self.token) {
            return String::new();
        }
        // Keep the logical token after publishing ordinary text: a following '=' still
        // needs its key, and a following credential prefix still needs its boundary.
        self.controls.clear();
        std::mem::take(&mut self.raw_token)
    }

    pub fn finish(&mut self) -> String {
        if self.finished {
            return String::new();
        }
        self.finished = true;
        let tail = if self.pem == Some(Pem::Header) {
            std::mem::take(&mut self.pem_tail)
        } else {
            self.pem_tail.clear();
            String::new()
        };
        tail + &self.flush_token()
    }

    fn is_secret(&self, token: &str) -> bool {
        (self.bearer && is_bearer_value(token))
            || PREFIX.is_match(token)
            || CLOUD_KEY.is_match(token)
            || is_jwt(token)
    }

    fn started_prefix(&self) -> Option<String> {
        if self.bearer && !self.token.is_empty() {
            return Some(String::new());
        }
        STARTED_PREFIX
            .find(&self.token)
            .map(|found| self.token[..found.end()].to_owned())
    }

    fn flush_token(&mut self) -> String {
        if self.oversized {
            self.oversized = false;
            self.bearer = false;
            return String::new();
        }
        let prefix = if self.finished {
            self.started_prefix()
        } else {
            None
        };
        let token = std::mem::take(&mut self.token);
        let raw_token = std::mem::take(&mut self.raw_token);
        let controls = std::mem::take(&mut self.controls);
        if token.is_empty() {
            return raw_token;
        }
        let secret = prefix.as_deref() != Some(token.as_str()) && self.is_secret(&token);
        self.bearer = !secret && token.eq_ignore_ascii_case("Bearer");
        if secret {
            return controls + REDACTED;
        }
        if let Some(prefix) = prefix {
            let published =
                char_len(&token).saturating_sub(char_len(&raw_token) - char_len(&controls));
            let rest: String = prefix.chars().skip(published).collect();
            return controls + &rest + REDACTED;
        }
        raw_token
    }
}

/// Per-stream redactor plus an incremental UTF-8 decoder (senpi's `{ redactor, decoder }` map entry).
#[derive(Default)]
pub struct RedactedStream {
    redactor: SecretOutputRedactor,
    pending: Vec<u8>,
}
impl RedactedStream {
    /// `TextDecoder.decode(data, { stream: true })`: holds an incomplete trailing sequence, U+FFFD for invalid bytes.
    fn decode(&mut self, data: &[u8]) -> String {
        self.pending.extend_from_slice(data);
        let mut decoded = String::new();
        let mut consumed = 0;
        while consumed < self.pending.len() {
            match std::str::from_utf8(&self.pending[consumed..]) {
                Ok(text) => {
                    decoded.push_str(text);
                    consumed = self.pending.len();
                }
                Err(error) => {
                    let end = consumed + error.valid_up_to();
                    decoded.push_str(&String::from_utf8_lossy(&self.pending[consumed..end]));
                    consumed = end;
                    if let Some(length) = error.error_len() {
                        decoded.push('\u{fffd}');
                        consumed += length;
                    } else {
                        break;
                    }
                }
            }
        }
        self.pending.drain(..consumed);
        decoded
    }
    /// `TextDecoder.decode()`: flush a dangling partial sequence as U+FFFD.
    fn finish_decoder(&mut self) -> String {
        let tail = String::from_utf8_lossy(&self.pending).into_owned();
        self.pending.clear();
        tail
    }
}
impl RedactedStream {
    /// `redactor.push(decoder.decode(data, { stream: true }))`.
    pub fn push_bytes(&mut self, data: &[u8]) -> String {
        let text = self.decode(data);
        self.redactor.push(&text)
    }
    /// `redactor.push(decoder.decode()) + redactor.finish()`.
    pub fn finish_stream(&mut self) -> String {
        let tail = self.finish_decoder();
        self.redactor.push(&tail) + &self.redactor.finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Synthetic credentials assembled at runtime (senpi `bash-redaction-fixtures.ts` `secretOutputCases`).
    fn cases() -> Vec<(String, String, String)> {
        let b64url =
            |text: &str| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text.as_bytes());
        let mut cases: Vec<(String, String, String)> = Vec::new();
        for (index, prefix) in [
            "ghp_",
            "gho_",
            "ghs_",
            "github_pat_",
            "xoxa-",
            "xoxb-",
            "xoxp-",
            "xoxo-",
            "xoxr-",
            "xoxs-",
            "sk-",
            "sk-ant-",
            "sk-proj-",
        ]
        .into_iter()
        .enumerate()
        {
            cases.push((
                format!("prefix {index}"),
                format!("{prefix}{}", "synthetic123".repeat(4)),
                "<redacted>".into(),
            ));
        }
        cases.push((
            "AWS".into(),
            format!("AKIA{}", "A1".repeat(8)),
            "<redacted>".into(),
        ));
        cases.push((
            "Google".into(),
            format!("AIza{}", "a".repeat(35)),
            "<redacted>".into(),
        ));
        cases.push((
            "JWT".into(),
            format!(
                "{}.{}.signature123",
                b64url(r#"{"alg":"HS256"}"#),
                b64url(r#"{"sub":"test"}"#)
            ),
            "<redacted>".into(),
        ));
        cases.push((
            "PEM".into(),
            format!(
                "-----BEGIN RSA PRIVATE KEY-----\n{}\n-----END RSA PRIVATE KEY-----",
                "c3ludGhldGljIGtleSBtYXRlcmlhbA=="
            ),
            "<redacted>".into(),
        ));
        cases.push((
            "Bearer".into(),
            "Bearer synthetic-value123".into(),
            "Bearer <redacted>".into(),
        ));
        for key in [
            "API_KEY",
            "ACCESS_TOKEN",
            "CLIENT_SECRET",
            "PASSWORD",
            "DB_PASSWORD_FILE",
            "HTTP_AUTHORIZATION",
        ] {
            cases.push((
                key.into(),
                format!("{key}=synthetic-value123"),
                format!("{key}=<redacted>"),
            ));
        }
        cases.push((
            "quoted assignment".into(),
            "DB_PASSWORD=\"synthetic multi word value\"".into(),
            "DB_PASSWORD=\"<redacted>\"".into(),
        ));
        cases
    }

    fn char_by_char(input: &str) -> String {
        let mut redactor = SecretOutputRedactor::new();
        let mut output: String = input
            .chars()
            .map(|c| redactor.push(&c.to_string()))
            .collect();
        output.push_str(&redactor.finish());
        output
    }

    #[test]
    fn redacts_every_fixture_whole_and_char_by_char() {
        for (name, input, expected) in cases() {
            let text = format!("{input} done\n");
            let want = format!("{expected} done\n");
            let mut redactor = SecretOutputRedactor::new();
            let whole = redactor.push(&text) + &redactor.finish();
            assert_eq!(whole, want, "whole {name}");
            assert_eq!(char_by_char(&text), want, "split {name}");
        }
    }

    #[test]
    fn redacted_output_is_stable_across_a_second_pass() {
        let input = cases()
            .into_iter()
            .map(|(_, _, expected)| expected)
            .collect::<Vec<_>>()
            .join("\n");
        let mut redactor = SecretOutputRedactor::new();
        assert_eq!(redactor.push(&input) + &redactor.finish(), input);
    }

    #[test]
    fn withholds_cloud_jwt_and_bearer_bodies_across_idle_pauses() {
        for name in ["AWS", "Google", "JWT", "Bearer"] {
            let (_, input, expected) = cases()
                .into_iter()
                .find(|case| case.0 == name)
                .expect("case");
            let split = if name == "JWT" {
                input.rfind('.').expect("dot")
            } else {
                input.len() - 1
            };
            let visible = if name == "Bearer" { "Bearer " } else { "" };
            let mut redactor = SecretOutputRedactor::new();
            assert_eq!(
                redactor.push(&input[..split]) + &redactor.flush_idle(),
                visible
            );
            assert_eq!(redactor.flush_idle(), "");
            assert_eq!(redactor.push(&input[split..]) + &redactor.flush_idle(), "");
            assert_eq!(
                visible.to_owned() + &redactor.push(" done\n") + &redactor.finish(),
                format!("{expected} done\n")
            );
        }
    }

    #[test]
    fn releases_ordinary_idle_fragments_verbatim() {
        for input in [
            "I", "e.g.", "git", "sk", "AI", "s", "g", "x", "A", "AK", "e",
        ] {
            let mut redactor = SecretOutputRedactor::new();
            assert_eq!(redactor.push(input) + &redactor.flush_idle(), input);
            assert_eq!(redactor.finish(), "");
        }
    }

    #[test]
    fn sanitizes_an_unfinished_public_prefix_at_eof() {
        for prefix in [
            "ghp_",
            "gho_",
            "ghs_",
            "ghu_",
            "ghr_",
            "github_pat_",
            "xoxa-",
            "xoxb-",
            "xoxp-",
            "xoxo-",
            "xoxr-",
            "xoxs-",
            "sk-",
            "sk-ant-",
            "sk-proj-",
            "AKIA",
            "AIza",
            "eyJ",
        ] {
            let mut redactor = SecretOutputRedactor::new();
            assert_eq!(redactor.push(prefix) + &redactor.flush_idle(), "");
            assert_eq!(redactor.finish(), format!("{prefix}<redacted>"));
        }
    }

    #[test]
    fn a_secret_assignment_body_is_never_published_at_idle_or_eof() {
        let mut redactor = SecretOutputRedactor::new();
        assert_eq!(redactor.push("API_KEY") + &redactor.flush_idle(), "API_KEY");
        assert_eq!(
            redactor.push("=synthetic") + &redactor.flush_idle(),
            "=<redacted>"
        );
        assert_eq!(
            redactor.push("-tail") + &redactor.flush_idle() + &redactor.finish(),
            ""
        );
    }

    #[test]
    fn author_metadata_is_not_an_auth_key() {
        let mut redactor = SecretOutputRedactor::new();
        let input = "GIT_AUTHOR_NAME=Jane Doe\nAUTH=private\nHTTP_AUTHORIZATION=private\n";
        assert_eq!(
            redactor.push(input) + &redactor.finish(),
            "GIT_AUTHOR_NAME=Jane Doe\nAUTH=<redacted>\nHTTP_AUTHORIZATION=<redacted>\n"
        );
    }

    #[test]
    fn bearer_requires_a_token_shaped_value() {
        let mut redactor = SecretOutputRedactor::new();
        let input = "the bearer of this\nBearer ordinary words\n";
        assert_eq!(redactor.push(input) + &redactor.finish(), input);
    }

    #[test]
    fn unclosed_quote_values_end_at_a_newline() {
        for quote in ['\'', '"'] {
            let input = format!("PASSWORD={quote}\nline2\nline3");
            assert_eq!(
                char_by_char(&input),
                format!("PASSWORD={quote}<redacted>\nline2\nline3")
            );
        }
    }

    #[test]
    fn quoted_assignment_survives_escaped_quotes_and_unquoted_punctuation() {
        assert_eq!(
            char_by_char("MY_SECRET=\"one \\\" two three\" after"),
            "MY_SECRET=\"<redacted>\" after"
        );
        let mut redactor = SecretOutputRedactor::new();
        assert_eq!(
            redactor.push("PASSWORD=synthetic,comma;tail/path after") + &redactor.finish(),
            "PASSWORD=<redacted> after"
        );
    }

    #[test]
    fn recognizes_tokens_across_ansi_and_discards_osc_payloads() {
        let token = cases().remove(0).1;
        let input = format!(
            "{}\x1b[31m{}\x1b[0m\n\x1b]0;{token}\x07done",
            &token[..2],
            &token[2..]
        );
        let output = char_by_char(&input);
        let plain = Regex::new(r"\x1b\[[0-9;]*m")
            .expect("regex")
            .replace_all(&output, "");
        assert_eq!(plain, "<redacted>\ndone");
    }

    #[test]
    fn consumes_the_full_charset_designation() {
        let token = cases().remove(0).1;
        for designator in ['(', ')', '*', '+'] {
            let control = format!("\x1b{designator}B\x1b[m");
            assert_eq!(
                char_by_char(&format!("{control}{token}\n")),
                format!("{control}<redacted>\n")
            );
        }
    }

    #[test]
    fn redacts_an_unterminated_pem_block_at_eof() {
        let input = "-----BEGIN RSA PRIVATE KEY-----\nc3ludGhldGljIGtleSBtYXRlcmlhbA==";
        let mut redactor = SecretOutputRedactor::new();
        assert_eq!(redactor.push(input) + &redactor.finish(), "<redacted>");
    }

    #[test]
    fn redacts_an_arbitrarily_large_undecided_token_without_a_suffix() {
        let mut redactor = SecretOutputRedactor::new();
        let mut output = String::new();
        for _ in 0..100 {
            output.push_str(&redactor.push(&"a".repeat(4096)));
        }
        output.push_str(&redactor.push("\nend"));
        output.push_str(&redactor.finish());
        assert_eq!(output, "<redacted>\nend");
        assert_eq!(redactor.finish(), "");
    }

    #[test]
    fn keeps_ordinary_text_and_unicode_unchanged() {
        let input = "build v1.2.3 /tmp/out.txt COUNT=12\n\u{65e5}\u{672c}\u{8a9e} \u{1f431}\n";
        assert_eq!(char_by_char(input), input);
    }

    #[test]
    fn redacts_padded_bearer_and_jwt_headers_with_whitespace() {
        let header =
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{ "alg": "HS256" }"#);
        let input = format!("Bearer abc=def==\n{header}.payload.signature\n");
        assert_eq!(char_by_char(&input), "Bearer <redacted>\n<redacted>\n");
    }

    #[test]
    fn redacted_stream_decodes_split_utf8_incrementally() {
        let mut stream = RedactedStream::default();
        let bytes = "caf\u{e9}".as_bytes();
        let first = stream.push_bytes(&bytes[..4]);
        let second = stream.push_bytes(&bytes[4..]);
        assert_eq!(first + &second + &stream.finish_stream(), "caf\u{e9}");
    }
}
