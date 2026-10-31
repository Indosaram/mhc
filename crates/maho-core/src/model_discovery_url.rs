//! Port of senpi packages/coding-agent/src/core/model-discovery-url.ts.
//!
//! Listing URL for `models discover` (senpi#2196): the request keeps the configured URL exactly, while
//! everything printed uses a display form with the userinfo removed and every query value redacted,
//! because a configured base URL may carry a key. maho-core has no URL parser dependency (the CLI's
//! `listing_url` uses `url::Url`); this splits `scheme://authority/path?query#fragment` by hand and
//! accepts only absolute `scheme://` URLs.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListingUrl {
    /// Sent on the wire, unchanged apart from the `/models` path segment.
    pub request: String,
    /// Safe to print.
    pub display: String,
}

struct Parts<'a> {
    scheme: &'a str,
    authority: &'a str,
    path: &'a str,
    query: Option<&'a str>,
}

fn split(url: &str) -> Option<Parts<'_>> {
    let (scheme, rest) = url.split_once("://")?;
    if scheme.is_empty()
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return None;
    }
    let rest = rest.split('#').next().unwrap_or_default();
    let authority_end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (authority, rest) = rest.split_at(authority_end);
    let host = authority.rsplit('@').next().unwrap_or_default();
    if host.is_empty() || host.contains(char::is_whitespace) {
        return None;
    }
    let (path, query) = match rest.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (rest, None),
    };
    Some(Parts {
        scheme,
        authority,
        path,
        query,
    })
}

fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            )
        {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn decode_component(value: &str) -> String {
    let bytes = value.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && let Some(byte) =
                std::str::from_utf8(bytes.get(index + 1..index + 3).unwrap_or_default())
                    .ok()
                    .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The URL without userinfo or fragment, every query value replaced by `<redacted>`.
pub fn display_url(url: &str) -> Option<String> {
    let parts = split(url)?;
    let host = parts.authority.rsplit('@').next().unwrap_or_default();
    let path = if parts.path.is_empty() {
        "/"
    } else {
        parts.path
    };
    let query: Vec<String> = parts
        .query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            format!(
                "{}=<redacted>",
                encode_component(&decode_component(
                    pair.split('=').next().unwrap_or_default()
                ))
            )
        })
        .collect();
    let query = if query.is_empty() {
        String::new()
    } else {
        format!("?{}", query.join("&"))
    };
    Some(format!(
        "{}://{host}{path}{query}",
        parts.scheme.to_ascii_lowercase()
    ))
}

/// `<baseUrl>/models`, or `None` when the base URL does not parse.
pub fn listing_url(base_url: &str) -> Option<ListingUrl> {
    let trimmed = base_url.trim();
    let parts = split(trimmed)?;
    let path = format!("{}/models", parts.path.trim_end_matches('/'));
    let query = parts
        .query
        .map(|query| format!("?{query}"))
        .unwrap_or_default();
    // WHATWG URL serialization lowercases the host; the userinfo keeps its case.
    let authority = match parts.authority.rsplit_once('@') {
        Some((userinfo, host)) => format!("{userinfo}@{}", host.to_ascii_lowercase()),
        None => parts.authority.to_ascii_lowercase(),
    };
    let request = format!(
        "{}://{authority}{path}{query}",
        parts.scheme.to_ascii_lowercase()
    );
    let display = display_url(&request)?;
    Some(ListingUrl { request, display })
}

/// An error message with the request URL, and any other userinfo, replaced by safe text.
pub fn redact_url_in_message(message: &str, url: &ListingUrl) -> String {
    let replaced = message.replace(&url.request, &url.display);
    let mut out = String::with_capacity(replaced.len());
    let mut rest = replaced.as_str();
    while let Some(index) = rest.find("//") {
        out.push_str(&rest[..index + 2]);
        rest = &rest[index + 2..];
        let userinfo_end = rest.find(|c: char| c == '/' || c == '@' || c.is_whitespace());
        if let Some(end) = userinfo_end
            && end > 0
            && rest[end..].starts_with('@')
        {
            rest = &rest[end + 1..];
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_request_exact_and_redacts_the_display() {
        let Some(url) = listing_url(" https://User:pw@Example.com/v1/?key=secret&org=acme ") else {
            panic!("parses")
        };
        assert_eq!(
            url.request,
            "https://User:pw@example.com/v1/models?key=secret&org=acme"
        );
        assert_eq!(
            url.display,
            "https://example.com/v1/models?key=<redacted>&org=<redacted>"
        );
        assert_eq!(
            listing_url("http://localhost:8080")
                .map(|u| u.display)
                .as_deref(),
            Some("http://localhost:8080/models")
        );
        assert_eq!(listing_url("not a url"), None);
    }

    #[test]
    fn redacts_the_request_and_any_userinfo_in_messages() {
        let Some(url) = listing_url("https://u:p@h/v1?k=s") else {
            panic!("parses")
        };
        let message = format!("GET {} failed; also see https://a:b@other/x", url.request);
        assert_eq!(
            redact_url_in_message(&message, &url),
            "GET https://h/v1/models?k=<redacted> failed; also see https://other/x"
        );
    }
}
