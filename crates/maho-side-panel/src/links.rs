//! The private `mhc-panel:` link scheme (spec 7).

/// URL scheme claimed by the panel.
pub const SCHEME: &str = "mhc-panel";
/// Value used by the `goal` and `memory` kinds.
pub const CURRENT: &str = "current";

/// A clickable target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    File(String),
    Agent(String),
    Goal,
    Memory,
}

fn encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = value.get(i + 1..i + 3)?;
            if !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

impl Link {
    /// The link URL; every byte outside the unreserved set is percent-encoded.
    pub fn url(&self) -> String {
        let (kind, value) = match self {
            Self::File(path) => ("file", encode(path)),
            Self::Agent(id) => ("agent", encode(id)),
            Self::Goal => ("goal", CURRENT.into()),
            Self::Memory => ("memory", CURRENT.into()),
        };
        format!("{SCHEME}:{kind}/{value}")
    }

    /// Parses a URL the panel claims; anything else returns `None` (goes to the previous handler).
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url.strip_prefix(SCHEME)?.strip_prefix(':')?;
        let (kind, raw) = rest.split_once('/')?;
        if raw.is_empty() {
            return None;
        }
        let value = decode(raw)?;
        if value.is_empty() {
            return None;
        }
        match kind {
            "file" => Some(Self::File(value)),
            "agent" => Some(Self::Agent(value)),
            "goal" if value == CURRENT => Some(Self::Goal),
            "memory" if value == CURRENT => Some(Self::Memory),
            _ => None,
        }
    }
}

/// Wraps `text` in an OSC 8 hyperlink to `link`.
pub fn hyperlink(link: &Link, text: &str) -> String {
    format!("\x1b]8;;{}\x1b\\{text}\x1b]8;;\x1b\\", link.url())
}

/// Whether the terminal likely supports OSC 8 hyperlinks (from the environment).
pub fn terminal_supports_links(env: &dyn Fn(&str) -> Option<String>) -> bool {
    if env("MAHO_NO_HYPERLINKS").is_some() {
        return false;
    }
    if env("TERM").as_deref() == Some("dumb") {
        return false;
    }
    true
}
