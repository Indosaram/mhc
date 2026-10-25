use std::collections::HashMap;
use std::sync::{Mutex, OnceLock, PoisonError};

fn registry() -> &'static Mutex<HashMap<String, String>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    REGISTRY.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Version-00 W3C traceparent with lowercase, nonzero trace and span IDs.
pub fn is_traceparent(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 55
        && &bytes[..3] == b"00-"
        && bytes[35] == b'-'
        && bytes[52] == b'-'
        && [&bytes[3..35], &bytes[36..52], &bytes[53..55]]
            .iter()
            .all(|part| {
                part.iter()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(b))
            })
        && bytes[3..35].iter().any(|b| *b != b'0')
        && bytes[36..52].iter().any(|b| *b != b'0')
}

pub fn publish_turn_traceparent(session_id: &str, traceparent: &str) {
    if is_traceparent(traceparent) {
        registry()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(session_id.into(), traceparent.into());
    }
}

pub fn clear_turn_traceparent(session_id: &str, expected: Option<&str>) {
    let mut map = registry().lock().unwrap_or_else(PoisonError::into_inner);
    if expected.is_none() || map.get(session_id).map(String::as_str) == expected {
        map.remove(session_id);
    }
}

pub fn read_turn_traceparent(session_id: &str) -> Option<String> {
    registry()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(session_id)
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    const VALID: &str = "00-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-00";

    #[test]
    fn validates_version_case_and_nonzero_ids() {
        assert!(is_traceparent(VALID));
        for invalid in [
            "",
            "not-a-traceparent",
            "00-00000000000000000000000000000000-b7ad6b7169203331-01",
            "00-0af7651916cd43dd8448eb211c80319c-0000000000000000-01",
            "01-0af7651916cd43dd8448eb211c80319c-b7ad6b7169203331-01",
        ] {
            assert!(!is_traceparent(invalid));
        }
        assert!(!is_traceparent(&VALID.to_uppercase()));
        assert!(!is_traceparent(&format!("{VALID}\n")));
    }

    #[test]
    fn publication_ignores_invalid_values_and_clear_is_conditional() {
        publish_turn_traceparent("bridge-test", VALID);
        publish_turn_traceparent("bridge-test", "invalid");
        assert_eq!(read_turn_traceparent("bridge-test").as_deref(), Some(VALID));
        clear_turn_traceparent("bridge-test", Some("old"));
        assert_eq!(read_turn_traceparent("bridge-test").as_deref(), Some(VALID));
        clear_turn_traceparent("bridge-test", Some(VALID));
        assert_eq!(read_turn_traceparent("bridge-test"), None);
        publish_turn_traceparent("bridge-test", VALID);
        clear_turn_traceparent("bridge-test", None);
        assert_eq!(read_turn_traceparent("bridge-test"), None);
    }
}
