//! Thread link format (`thread/thread-link.ts`).

pub const THREAD_LINK_SCHEME: &str = "omo-thread";
const PREFIX: &str = "omo-thread://v1/";
const FALLBACK_LABEL: &str = "thread";

fn encode_uri_component(input: &str) -> String {
    // Matches JS `encodeURIComponent` for UTF-8 bytes: unreserved A-Za-z0-9 - _ . ! ~ * ' ( )
    let mut out = String::with_capacity(input.len());
    for byte in input.as_bytes() {
        let keep = matches!(
            *byte,
            b'A'..=b'Z'
                | b'a'..=b'z'
                | b'0'..=b'9'
                | b'-'
                | b'_'
                | b'.'
                | b'!'
                | b'~'
                | b'*'
                | b'\''
                | b'('
                | b')'
        );
        if keep {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

fn decode_uri_component(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let val = u8::from_str_radix(hex, 16).ok()?;
            out.push(val);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

pub fn thread_link_url(thread_id: &str) -> String {
    format!("{PREFIX}{}", encode_uri_component(thread_id))
}

/// `[label](omo-thread://v1/<id>)`, escaping `\`, `[`, `]`, `<`, `>`, `&`, `` ` ``.
pub fn thread_link(thread_id: &str, label: Option<&str>) -> String {
    let collapsed = label
        .map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| FALLBACK_LABEL.to_owned());
    let mut escaped = String::with_capacity(collapsed.len());
    for ch in collapsed.chars() {
        if matches!(ch, '\\' | '[' | ']' | '<' | '>' | '&' | '`') {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    format!("[{escaped}]({})", thread_link_url(thread_id))
}

/// The thread id a link names, or `None` for anything that is not a v1 thread link.
pub fn parse_thread_link_url(url: &str) -> Option<String> {
    let encoded = url.strip_prefix(PREFIX)?;
    if encoded.is_empty() || encoded.contains(['/', '?', '#']) {
        return None;
    }
    let decoded = decode_uri_component(encoded)?;
    (!decoded.is_empty()).then_some(decoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_and_parses_thread_links() {
        assert_eq!(
            thread_link("id-1", Some("  [A] & `B`  ")),
            "[\\[A\\] \\& \\`B\\`](omo-thread://v1/id-1)"
        );
        assert_eq!(thread_link("id-1", None), "[thread](omo-thread://v1/id-1)");
        assert_eq!(
            parse_thread_link_url("omo-thread://v1/id%2F1").as_deref(),
            Some("id/1")
        );
        assert!(parse_thread_link_url("omo-thread://v1/id/1").is_none());
    }
}
