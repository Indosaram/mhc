//! HTTP primitives for the release service.
//!
//! Upstream uses the WHATWG `Request`/`Response`/`Headers`/`URL` classes. This module
//! carries the small subset the service touches: a method, an absolute URL, a
//! case-insensitive header multimap, and a response with an optional body. Header
//! lookup and replacement are case-insensitive, matching `Headers`.

/// The request method, restricted to the two the service branches on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Head,
    Other,
}

impl Method {
    /// Parses a method token; anything other than `GET`/`HEAD` is `Other` (the 405 branch).
    #[must_use]
    pub fn parse(raw: &str) -> Self {
        match raw {
            "GET" => Method::Get,
            "HEAD" => Method::Head,
            _ => Method::Other,
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Head => "HEAD",
            Method::Other => "OTHER",
        }
    }
}

/// An inbound request: method, absolute URL, headers, and the Cloudflare `cf`
/// properties the service reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub method: Method,
    pub url: String,
    pub headers: Headers,
    pub cf_country: Option<String>,
}

impl Request {
    #[must_use]
    pub fn new(method: Method, url: impl Into<String>) -> Self {
        Request {
            method,
            url: url.into(),
            headers: Headers::new(),
            cf_country: None,
        }
    }

    #[must_use]
    pub fn with_headers(mut self, headers: Headers) -> Self {
        self.headers = headers;
        self
    }

    /// Sets `request.cf.country`; a host with no Cloudflare edge leaves it unset.
    #[must_use]
    pub fn with_country(mut self, country: impl Into<String>) -> Self {
        self.cf_country = Some(country.into());
        self
    }

    /// The URL path, or `"/"` when no path can be found (mirrors `new URL().pathname`).
    #[must_use]
    pub fn pathname(&self) -> String {
        pathname_of(&self.url).unwrap_or_else(|| "/".to_string())
    }

    #[must_use]
    pub fn has_header(&self, name: &str) -> bool {
        self.headers.contains(name)
    }

    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name)
    }
}

/// Case-insensitive header multimap preserving insertion order, like `Headers`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Headers {
    entries: Vec<(String, String)>,
}

impl Headers {
    #[must_use]
    pub fn new() -> Self {
        Headers {
            entries: Vec::new(),
        }
    }

    /// Replaces an existing entry in place or appends (matches `Headers.set`).
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(slot) = self
            .entries
            .iter_mut()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
        {
            slot.1 = value;
            return;
        }
        self.entries.push((name.to_string(), value));
    }

    pub fn append(&mut self, name: &str, value: impl Into<String>) {
        self.entries.push((name.to_string(), value.into()));
    }

    #[must_use]
    pub fn get(&self, name: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    #[must_use]
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// A response body: opaque bytes or UTF-8 text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Bytes(Vec<u8>),
    Text(String),
}

impl Body {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Body::Bytes(bytes) => bytes,
            Body::Text(text) => text.as_bytes(),
        }
    }

    #[must_use]
    pub fn text(&self) -> String {
        match self {
            Body::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
            Body::Text(text) => text.clone(),
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Body::Bytes(bytes) => bytes.is_empty(),
            Body::Text(text) => text.is_empty(),
        }
    }
}

/// An outbound response: status, headers, optional body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Response {
    pub status: u16,
    pub headers: Headers,
    pub body: Option<Body>,
}

impl Response {
    #[must_use]
    pub fn new(status: u16, body: Option<Body>) -> Self {
        Response {
            status,
            headers: Headers::new(),
            body,
        }
    }

    #[must_use]
    pub fn with_header(mut self, name: &str, value: impl Into<String>) -> Self {
        self.headers.set(name, value);
        self
    }

    /// Copies every header of `headers` over this response (matches `new Headers(cached.headers)`).
    #[must_use]
    pub fn with_headers(mut self, headers: &Headers) -> Self {
        for (name, value) in headers.iter() {
            self.headers.set(name, value);
        }
        self
    }

    #[must_use]
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }

    #[must_use]
    pub fn body_bytes(&self) -> &[u8] {
        self.body.as_ref().map_or(&[], Body::as_bytes)
    }

    #[must_use]
    pub fn body_text(&self) -> String {
        self.body.as_ref().map(Body::text).unwrap_or_default()
    }

    /// Drops the body but keeps the status and headers (the HEAD response shape).
    #[must_use]
    pub fn without_body(mut self) -> Self {
        self.body = None;
        self
    }
}

/// Extracts the path from an absolute or origin-form URL, mirroring `URL.pathname`.
///
/// Returns `None` only when no path segment can be located; callers fall back to `"/"`.
#[must_use]
pub fn pathname_of(url: &str) -> Option<String> {
    let after_scheme = match url.split_once("://") {
        Some((_, rest)) => rest,
        None => url,
    };
    let path = match after_scheme.find('/') {
        Some(index) => &after_scheme[index..],
        None => return Some("/".to_string()),
    };
    let end = path.find(['?', '#']).unwrap_or(path.len());
    Some(path[..end].to_string())
}
