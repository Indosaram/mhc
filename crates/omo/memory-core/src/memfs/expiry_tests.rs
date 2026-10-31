use super::{is_memory_expired, memory_expires_at_ms};
use crate::memfs::frontmatter::parse_memory_file;

const DAY: i64 = 86_400_000;
// 2026-10-27T00:00Z
const OCT_27: i64 = 20_753 * DAY;
const OCT_27_0633: i64 = OCT_27 + (6 * 60 + 33) * 60_000;

fn file(expires_line: &str) -> String {
    format!("---\ndescription: Temporary note\n{expires_line}\n---\nbody\n")
}

#[test]
fn reads_the_declared_expiry_instant() {
    let cases: &[(&str, &str, Option<i64>)] = &[
        (
            "date is the start of that UTC day",
            "expires: 2026-10-28",
            Some(OCT_27 + DAY),
        ),
        (
            "UTC instant with minutes",
            "expires: 2026-10-27T06:33Z",
            Some(OCT_27_0633),
        ),
        (
            "UTC instant with seconds and fraction",
            "expires: 2026-10-27T06:33:15.250Z",
            Some(OCT_27_0633 + 15_250),
        ),
        (
            "positive offset",
            "expires: 2026-10-27T15:33+09:00",
            Some(OCT_27_0633),
        ),
        (
            "negative offset",
            "expires: 2026-10-26T23:33:00-07:00",
            Some(OCT_27_0633),
        ),
        (
            "double-quoted instant",
            "expires: \"2026-10-27T06:33Z\"",
            Some(OCT_27_0633),
        ),
        (
            "single-quoted date",
            "expires: '2026-10-28'",
            Some(OCT_27 + DAY),
        ),
        (
            "offset-less datetime never expires",
            "expires: 2026-10-27T06:33",
            None,
        ),
        (
            "space-separated time is not ISO 8601",
            "expires: 2026-10-27 06:33Z",
            None,
        ),
        ("impossible calendar date", "expires: 2026-02-30", None),
        ("impossible hour", "expires: 2026-10-27T25:00Z", None),
        ("free text", "expires: after the outage", None),
        ("empty value", "expires: ", None),
        ("no expires key", "kind: note", None),
    ];
    for (name, line, expected) in cases {
        let parsed = parse_memory_file(&file(line)).expect("frontmatter");
        assert_eq!(
            memory_expires_at_ms(&parsed.frontmatter),
            *expected,
            "{name}"
        );
    }
}

#[test]
fn a_file_is_expired_from_its_instant_on() {
    assert!(!is_memory_expired(None, i64::MAX));
    assert!(!is_memory_expired(Some(10), 9));
    assert!(is_memory_expired(Some(10), 10));
}
