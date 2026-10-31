//! Network snoop entries to a HAR 1.2 log.
//!
//! Port of omowright `src/har.js` (`toHar`): pure, touches no disk; header order follows the CDP
//! wire order, query strings are decoded the way `URLSearchParams` decodes them, failed entries get
//! status 0 and `_errorText`, and unfinished or clock-skewed entries never get a negative time.
use super::network_snoop::NetworkEntry;
use serde_json::{Map, Value, json};

const HTTP_VERSION: &str = "HTTP/1.1";

/// Creator written when none is given (omowright keeps it in sync with its package manifest).
pub fn default_creator() -> Value {
    json!({"name": "omowright", "version": "0.0.1"})
}

/// `toHar` options.
#[derive(Clone, Debug, Default)]
pub struct HarOptions {
    /// Defaults to [`default_creator`].
    pub creator: Option<Value>,
    pub pages: Vec<Value>,
}

fn value_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

fn header_list(headers: Option<&Map<String, Value>>) -> Value {
    Value::Array(
        headers
            .into_iter()
            .flatten()
            .map(|(name, value)| json!({"name": name, "value": value_text(value)}))
            .collect(),
    )
}

fn header_value(headers: Option<&Map<String, Value>>, name: &str) -> Option<String> {
    headers?
        .iter()
        .find(|(key, _)| key.to_ascii_lowercase() == name)
        .map(|(_, value)| value_text(value))
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// `application/x-www-form-urlencoded` decoding (`+` is a space, bad escapes stay literal).
fn form_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => out.push(b' '),
            b'%' if index + 2 < bytes.len() => {
                match (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                    (Some(high), Some(low)) => {
                        out.push(high * 16 + low);
                        index += 2;
                    }
                    _ => out.push(b'%'),
                }
            }
            byte => out.push(byte),
        }
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Whether `url` is absolute (`scheme:` prefix), as `new URL(url)` requires.
fn is_absolute(url: &str) -> bool {
    let Some((scheme, _)) = url.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

fn query_string(url: &str) -> Value {
    if !is_absolute(url) {
        // Non-absolute or malformed urls carry no parsable query.
        return json!([]);
    }
    let without_fragment = url.split('#').next().unwrap_or_default();
    let Some((_, query)) = without_fragment.split_once('?') else {
        return json!([]);
    };
    Value::Array(
        query
            .split('&')
            .filter(|pair| !pair.is_empty())
            .map(|pair| {
                let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
                json!({"name": form_decode(name), "value": form_decode(value)})
            })
            .collect(),
    )
}

fn elapsed(entry: &NetworkEntry) -> i64 {
    entry
        .finished_at
        .map_or(0, |finished| (finished - entry.started_at).max(0))
}

/// Days since 1970-01-01 to a civil (year, month, day) date.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// `new Date(ms).toISOString()`.
pub fn iso_timestamp(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rest = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3_600_000,
        rest / 60_000 % 60,
        rest / 1000 % 60,
        rest % 1000
    )
}

fn request_of(entry: &NetworkEntry) -> Value {
    let mut request = json!({
        "method": entry.method,
        "url": entry.url,
        "httpVersion": HTTP_VERSION,
        "cookies": [],
        "headers": header_list(Some(&entry.request_headers)),
        "queryString": query_string(&entry.url),
        "headersSize": -1,
        "bodySize": entry.post_data.as_ref().map_or(0, String::len),
    });
    if let Some(post_data) = &entry.post_data {
        request["postData"] = json!({
            "mimeType": header_value(Some(&entry.request_headers), "content-type").unwrap_or_default(),
            "text": post_data,
        });
    }
    request
}

fn content_of(entry: &NetworkEntry) -> Value {
    let mut content = json!({
        "size": entry.encoded_data_length,
        "mimeType": entry.mime_type.clone().unwrap_or_default(),
    });
    if let Some(body) = &entry.body {
        content["text"] = json!(body);
    }
    if entry.base64_encoded {
        content["encoding"] = json!("base64");
    }
    content
}

fn response_of(entry: &NetworkEntry) -> Value {
    let headers = entry.response_headers.as_ref();
    json!({
        "status": if entry.failed { 0 } else { entry.status.unwrap_or(0) },
        "statusText": entry.status_text.clone().unwrap_or_default(),
        "httpVersion": HTTP_VERSION,
        "cookies": [],
        "headers": header_list(headers),
        "content": content_of(entry),
        "redirectURL": header_value(headers, "location").unwrap_or_default(),
        "headersSize": -1,
        "bodySize": entry.encoded_data_length,
    })
}

/// Converts network snoop entries into a HAR 1.2 log (`{ log: { ... } }`).
pub fn to_har(entries: &[NetworkEntry], options: &HarOptions) -> Value {
    let entries: Vec<Value> = entries
        .iter()
        .map(|entry| {
            let time = elapsed(entry);
            let mut har = json!({
                "startedDateTime": iso_timestamp(entry.started_at),
                "time": time,
                "request": request_of(entry),
                "response": response_of(entry),
                "cache": {},
                "timings": {"send": 0, "wait": time, "receive": 0},
            });
            if entry.failed {
                har["_errorText"] = json!(entry.error_text.clone().unwrap_or_default());
            }
            har
        })
        .collect();
    json!({
        "log": {
            "version": "1.2",
            "creator": options.creator.clone().unwrap_or_else(default_creator),
            "pages": options.pages,
            "entries": entries,
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON_BODY: &str = r#"{"ok":true}"#;

    fn headers(pairs: &[(&str, &str)]) -> Map<String, Value> {
        pairs
            .iter()
            .map(|(name, value)| ((*name).to_owned(), json!(value)))
            .collect()
    }

    fn entry() -> NetworkEntry {
        NetworkEntry {
            request_id: "r1".into(),
            url: "https://x.test/api/items?page=2&q=hello%20world".into(),
            method: "GET".into(),
            resource_type: Some("XHR".into()),
            status: Some(200),
            status_text: Some("OK".into()),
            mime_type: Some("application/json".into()),
            request_headers: headers(&[("accept", "*/*"), ("x-trace", "1")]),
            // serde_json maps may sort keys (no `preserve_order` here): fixtures use sorted names.
            response_headers: Some(headers(&[
                ("content-length", "11"),
                ("content-type", "application/json"),
            ])),
            remote_ip_address: None,
            post_data: None,
            encoded_data_length: 11,
            body: Some(JSON_BODY.into()),
            base64_encoded: false,
            body_skipped: false,
            failed: false,
            error_text: None,
            started_at: 1_700_000_000_000,
            finished_at: Some(1_700_000_000_250),
        }
    }

    fn first(entry: NetworkEntry) -> Value {
        to_har(&[entry], &HarOptions::default())["log"]["entries"][0].clone()
    }

    #[test]
    fn to_har_builds_a_har_1_2_log_with_creator_and_pages() {
        let har = to_har(&[entry()], &HarOptions::default());
        assert_eq!(har["log"]["version"], "1.2");
        assert_eq!(har["log"]["creator"]["name"], "omowright");
        assert!(har["log"]["creator"]["version"].is_string());
        assert_eq!(har["log"]["pages"], json!([]));
        assert_eq!(har["log"]["entries"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            to_har(&[], &HarOptions::default()),
            json!({"log": {"version": "1.2", "creator": har["log"]["creator"], "pages": [], "entries": []}})
        );
        let custom = to_har(
            &[],
            &HarOptions {
                creator: Some(json!({"name": "custom", "version": "9.9.9"})),
                pages: vec![json!({"id": "page_1"})],
            },
        );
        assert_eq!(
            custom["log"]["creator"],
            json!({"name": "custom", "version": "9.9.9"})
        );
        assert_eq!(custom["log"]["pages"], json!([{"id": "page_1"}]));
    }

    #[test]
    fn to_har_maps_a_request_onto_the_har_request_shape() {
        let har = first(entry());
        assert_eq!(har["startedDateTime"], "2023-11-14T22:13:20.000Z");
        assert_eq!(har["time"], 250);
        assert_eq!(har["request"]["method"], "GET");
        assert_eq!(
            har["request"]["url"],
            "https://x.test/api/items?page=2&q=hello%20world"
        );
        assert_eq!(har["request"]["httpVersion"], "HTTP/1.1");
        assert_eq!(har["request"]["cookies"], json!([]));
        assert_eq!(
            har["request"]["headers"],
            json!([{"name": "accept", "value": "*/*"}, {"name": "x-trace", "value": "1"}])
        );
        assert_eq!(
            har["request"]["queryString"],
            json!([{"name": "page", "value": "2"}, {"name": "q", "value": "hello world"}])
        );
        assert_eq!(har["request"]["headersSize"], -1);
        assert_eq!(har["request"]["bodySize"], 0);
        assert!(har["request"].get("postData").is_none());
        assert_eq!(har["cache"], json!({}));
        assert_eq!(
            har["timings"],
            json!({"send": 0, "wait": 250, "receive": 0})
        );
    }

    #[test]
    fn to_har_maps_a_response_onto_the_har_response_shape() {
        let har = first(entry());
        let response = &har["response"];
        assert_eq!(response["status"], 200);
        assert_eq!(response["statusText"], "OK");
        assert_eq!(response["httpVersion"], "HTTP/1.1");
        assert_eq!(response["cookies"], json!([]));
        assert_eq!(
            response["headers"],
            json!([{"name": "content-length", "value": "11"}, {"name": "content-type", "value": "application/json"}])
        );
        assert_eq!(response["content"]["size"], 11);
        assert_eq!(response["content"]["mimeType"], "application/json");
        assert_eq!(response["content"]["text"], JSON_BODY);
        assert!(response["content"].get("encoding").is_none());
        assert_eq!(response["redirectURL"], "");
        assert_eq!(response["headersSize"], -1);
        assert_eq!(response["bodySize"], 11);
        assert!(har.get("_errorText").is_none());
    }

    #[test]
    fn to_har_flags_base64_bodies_and_carries_redirect_locations() {
        let har = first(NetworkEntry {
            url: "https://x.test/pixel.png".into(),
            mime_type: Some("image/png".into()),
            response_headers: Some(headers(&[
                ("content-type", "image/png"),
                ("location", "https://x.test/moved.png"),
            ])),
            status: Some(302),
            status_text: Some("Found".into()),
            body: Some("iVBORw0KGgo=".into()),
            base64_encoded: true,
            encoded_data_length: 8,
            ..entry()
        });
        assert_eq!(har["response"]["content"]["encoding"], "base64");
        assert_eq!(har["response"]["content"]["text"], "iVBORw0KGgo=");
        assert_eq!(har["response"]["content"]["mimeType"], "image/png");
        assert_eq!(har["response"]["redirectURL"], "https://x.test/moved.png");
        assert_eq!(har["request"]["queryString"], json!([]));
    }

    #[test]
    fn to_har_writes_post_data_with_the_request_content_type() {
        let har = first(NetworkEntry {
            method: "POST".into(),
            post_data: Some(r#"{"q":1}"#.into()),
            request_headers: headers(&[("content-type", "application/json; charset=utf-8")]),
            ..entry()
        });
        assert_eq!(har["request"]["bodySize"], 7);
        assert_eq!(
            har["request"]["postData"],
            json!({"mimeType": "application/json; charset=utf-8", "text": "{\"q\":1}"})
        );
    }

    #[test]
    fn to_har_gives_failed_entries_status_0_and_error_text() {
        let har = first(NetworkEntry {
            status: None,
            status_text: None,
            mime_type: None,
            response_headers: None,
            body: None,
            failed: true,
            error_text: Some("net::ERR_ABORTED".into()),
            encoded_data_length: 0,
            finished_at: Some(1_700_000_000_000),
            ..entry()
        });
        assert_eq!(har["response"]["status"], 0);
        assert_eq!(har["response"]["statusText"], "");
        assert_eq!(har["response"]["headers"], json!([]));
        assert_eq!(har["response"]["content"]["size"], 0);
        assert!(har["response"]["content"].get("text").is_none());
        assert_eq!(har["_errorText"], "net::ERR_ABORTED");
        assert_eq!(har["time"], 0);
    }

    #[test]
    fn to_har_never_emits_negative_times_for_unfinished_or_clock_skewed_entries() {
        let log = to_har(
            &[
                NetworkEntry {
                    finished_at: None,
                    ..entry()
                },
                NetworkEntry {
                    finished_at: Some(1_699_999_999_000),
                    ..entry()
                },
            ],
            &HarOptions::default(),
        );
        let entries = log["log"]["entries"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(entries.len(), 2);
        for har in entries {
            assert_eq!(har["time"], 0);
            assert_eq!(har["timings"], json!({"send": 0, "wait": 0, "receive": 0}));
        }
    }

    #[test]
    fn iso_timestamps_match_date_to_iso_string() {
        assert_eq!(iso_timestamp(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_timestamp(951_782_400_123), "2000-02-29T00:00:00.123Z");
        assert_eq!(iso_timestamp(-1), "1969-12-31T23:59:59.999Z");
    }
}
