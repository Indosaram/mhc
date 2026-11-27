//! Address parser & shape predicates (`packages/thread-address-core/src/parse.ts`).

use std::sync::LazyLock;

use regex::Regex;

pub const THREAD_ID_SHAPE: &str = r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$";
pub const SESSION_ID_SHAPE: &str = r"^(?:rpc-\d+|[A-Za-z0-9_-]{16,})$";
pub const ID_PREFIX_SHAPE: &str = r"^[0-9a-f]{8}[0-9a-f-]*$";

/// The shortest device id prefix a device part accepts: shorter would match most of an account's ids by chance.
pub const DEVICE_PREFIX_MIN: usize = 6;

/// The device part that names this device.
pub const HERE: &str = "here";

static THREAD_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
        .expect("valid static THREAD_ID_RE")
});
static SESSION_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:rpc-\d+|[A-Za-z0-9_-]{16,})$").expect("valid static SESSION_ID_RE")
});
static ROUTING_ID_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^rpc-\d+$").expect("valid static ROUTING_ID_RE"));
static LISTING_KEY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{22}$").expect("valid static LISTING_KEY_RE"));
static ID_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[0-9a-f]{8}[0-9a-f-]*$").expect("valid static ID_PREFIX_RE")
});
static DEVICE_THREAD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^([^/]+)/([^/]+)$").expect("valid static DEVICE_THREAD_RE"));
static DRIVE_LETTER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z]:$").expect("valid static DRIVE_LETTER_RE"));

pub fn is_thread_id_shape(text: &str) -> bool {
    THREAD_ID_RE.is_match(text)
}

pub fn is_session_id_shape(text: &str) -> bool {
    SESSION_ID_RE.is_match(text)
}

pub fn is_id_prefix_shape(text: &str) -> bool {
    ID_PREFIX_RE.is_match(text)
}

/// A thread id, a host routing id (`rpc-N`) or a device-service listing key: a text a device can
/// be asked for without listing it first (#13748).
pub fn id_shaped(text: &str) -> bool {
    is_thread_id_shape(text) || ROUTING_ID_RE.is_match(text) || LISTING_KEY_RE.is_match(text)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedAddress {
    SelfRef,
    Bare {
        text: String,
    },
    Device {
        device: String,
        thread: String,
        text: String,
    },
}

/// The one reading of an address text (design section 2). `<device>/<thread>` splits on its single `/`
/// unless the text is a path: a first part starting with `.` or `~`, a drive letter, or a `.jsonl` file.
pub fn parse_address(input: &str) -> ParsedAddress {
    let text = input.trim();
    if text == "self" {
        return ParsedAddress::SelfRef;
    }
    if let Some(caps) = DEVICE_THREAD_RE.captures(text)
        && let (Some(device_m), Some(thread_m)) = (caps.get(1), caps.get(2))
    {
        let device = device_m.as_str();
        let thread = thread_m.as_str();
        let is_jsonl = text.to_ascii_lowercase().ends_with(".jsonl");
        let is_path_segment = device.starts_with('.') || device.starts_with('~');
        if !is_jsonl && !DRIVE_LETTER_RE.is_match(device) && !is_path_segment {
            return ParsedAddress::Device {
                device: device.to_owned(),
                thread: thread.to_owned(),
                text: text.to_owned(),
            };
        }
    }
    ParsedAddress::Bare {
        text: text.to_owned(),
    }
}

/// `<device id>/<thread>`: the address every tool accepts for a thread on a known device.
pub fn format_address(device_id: &str, thread: &str) -> String {
    format!("{device_id}/{thread}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_self_bare_and_device_addresses() {
        assert_eq!(parse_address(" self "), ParsedAddress::SelfRef);
        assert_eq!(
            parse_address("here/my-thread"),
            ParsedAddress::Device {
                device: "here".into(),
                thread: "my-thread".into(),
                text: "here/my-thread".into()
            }
        );
        assert_eq!(
            parse_address("./session.jsonl"),
            ParsedAddress::Bare {
                text: "./session.jsonl".into()
            }
        );
        assert_eq!(
            parse_address("C:/foo"),
            ParsedAddress::Bare {
                text: "C:/foo".into()
            }
        );
        assert!(id_shaped("01965d50-1234-7000-8000-000000000001"));
        assert!(id_shaped("rpc-12"));
        assert!(!id_shaped("release-planner-2"));
    }

    #[test]
    fn preserves_paths_starting_with_dot_or_tilde_and_multi_slash_as_bare() {
        assert_eq!(
            parse_address("~/notes/todo"),
            ParsedAddress::Bare {
                text: "~/notes/todo".into()
            }
        );
        assert_eq!(
            parse_address("a/b/c"),
            ParsedAddress::Bare {
                text: "a/b/c".into()
            }
        );
        assert_eq!(format_address("dev-1", "thr-2"), "dev-1/thr-2");
    }

    #[test]
    fn recognizes_listing_key_ids_and_trimmed_self() {
        assert!(id_shaped("AbCdEfGhIjKlMnOpQrStUv"));
        assert_eq!(parse_address("self"), ParsedAddress::SelfRef);
    }
}
