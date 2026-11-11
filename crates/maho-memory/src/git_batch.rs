//! Parsing `git cat-file --batch` output for the reflection credential gate.
//!
//! Spec: `docs/cleanroom/specs/memory-git-batch.md`.

use std::collections::BTreeMap;

pub const SCANNED_BLOB_MODES: [&str; 3] = ["100644", "100755", "120000"];
pub const SUBMODULE_MODE: &str = "160000";

/// The declared size is the only way to find the next header when a body contains newlines, so any
/// inconsistency is an error: a partial map would read as "those blobs are clean".
pub fn parse_blob_bodies(output: &[u8]) -> Result<BTreeMap<String, String>, String> {
    let mut blobs = BTreeMap::new();
    let mut offset = 0usize;
    while offset < output.len() {
        let Some(newline_offset) = output[offset..].iter().position(|byte| *byte == b'\n') else {
            return Err("git cat-file --batch output ended inside a record header".to_string());
        };
        let newline = offset + newline_offset;
        let header = String::from_utf8_lossy(&output[offset..newline]).into_owned();
        offset = newline + 1;
        let mut fields = header.split(' ');
        let oid = fields.next().unwrap_or_default();
        let kind = fields.next();
        let Some(size_text) = fields.next() else {
            continue;
        };
        let Ok(size) = size_text.parse::<usize>() else {
            return Err(format!(
                "git cat-file --batch output is malformed at \"{header}\""
            ));
        };
        if oid.is_empty() || offset + size > output.len() {
            return Err(format!(
                "git cat-file --batch output is malformed at \"{header}\""
            ));
        }
        if kind == Some("blob") {
            blobs.insert(oid.to_string(), decode_blob(&output[offset..offset + size]));
        }
        offset += size + 1;
    }
    Ok(blobs)
}

/// Latin-1 keeps every byte distinct, so a binary cannot smuggle an ASCII credential by failing to decode.
pub fn decode_blob(body: &[u8]) -> String {
    match std::str::from_utf8(body) {
        Ok(text) => text.to_string(),
        Err(_) => body.iter().map(|byte| *byte as char).collect(),
    }
}

pub fn is_zero_sha(value: &str) -> bool {
    !value.is_empty() && value.chars().all(|ch| ch == '0')
}

pub fn short_sha(commit: &str) -> String {
    commit.chars().take(7).collect()
}

/// A gitlink is refused on its file type alone, so it is not a scanned mode.
pub fn is_scanned_blob_mode(mode: &str) -> bool {
    SCANNED_BLOB_MODES.contains(&mode)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(oid: &str, kind: &str, body: &[u8]) -> Vec<u8> {
        let mut out = format!("{oid} {kind} {}\n", body.len()).into_bytes();
        out.extend_from_slice(body);
        out.push(b'\n');
        out
    }

    #[test]
    fn one_blob_round_trips() {
        let blobs = parse_blob_bodies(&record("abc123", "blob", b"hello")).unwrap();
        assert_eq!(blobs.get("abc123").map(String::as_str), Some("hello"));
    }

    #[test]
    fn several_blobs_keep_their_own_bodies() {
        let mut output = record("a", "blob", b"first");
        output.extend(record("b", "blob", b"second"));
        let blobs = parse_blob_bodies(&output).unwrap();
        assert_eq!(blobs.get("a").map(String::as_str), Some("first"));
        assert_eq!(blobs.get("b").map(String::as_str), Some("second"));
    }

    #[test]
    fn a_body_containing_newlines_does_not_shift_later_records() {
        let mut output = record("a", "blob", b"line1\nline2\n\nline4");
        output.extend(record("b", "blob", b"after"));
        let blobs = parse_blob_bodies(&output).unwrap();
        assert_eq!(
            blobs.get("a").map(String::as_str),
            Some("line1\nline2\n\nline4")
        );
        assert_eq!(blobs.get("b").map(String::as_str), Some("after"));
    }

    #[test]
    fn a_body_that_looks_like_a_header_does_not_shift_later_records() {
        let mut output = record("a", "blob", b"fake 123 blob 5");
        output.extend(record("b", "blob", b"real"));
        let blobs = parse_blob_bodies(&output).unwrap();
        assert_eq!(blobs.get("b").map(String::as_str), Some("real"));
    }

    #[test]
    fn an_empty_body_is_kept_as_an_empty_string() {
        let blobs = parse_blob_bodies(&record("a", "blob", b"")).unwrap();
        assert_eq!(blobs.get("a").map(String::as_str), Some(""));
    }

    #[test]
    fn non_blob_records_are_skipped() {
        let mut output = record("t", "tree", b"binary-tree-ish");
        output.extend(record("b", "blob", b"text"));
        let blobs = parse_blob_bodies(&output).unwrap();
        assert!(!blobs.contains_key("t"));
        assert_eq!(blobs.get("b").map(String::as_str), Some("text"));
    }

    #[test]
    fn a_final_header_without_a_newline_is_reported() {
        let mut output = record("a", "blob", b"text");
        output.extend_from_slice(b"missing 123");
        let error = parse_blob_bodies(&output).unwrap_err();
        assert!(error.contains("record header"), "{error}");
    }

    #[test]
    fn a_truncated_body_is_an_error() {
        let mut output = b"a blob 10\n".to_vec();
        output.extend_from_slice(b"short");
        let error = parse_blob_bodies(&output).unwrap_err();
        assert!(error.contains("malformed"), "{error}");
    }

    #[test]
    fn a_truncated_header_is_an_error() {
        let error = parse_blob_bodies(b"aaaa blob").unwrap_err();
        assert!(error.contains("inside a record header"), "{error}");
    }

    #[test]
    fn an_empty_oid_is_an_error() {
        let error = parse_blob_bodies(b" blob 0\n\n").unwrap_err();
        assert!(error.contains("malformed"), "{error}");
    }

    #[test]
    fn empty_output_is_an_empty_map() {
        assert!(parse_blob_bodies(b"").unwrap().is_empty());
    }

    #[test]
    fn invalid_utf8_decodes_to_latin1_instead_of_failing() {
        let decoded = decode_blob(&[0xff, 0x41]);
        assert_eq!(decoded, "\u{ff}A");
        assert!(decoded.contains('A'));
    }

    #[test]
    fn an_invalid_utf8_blob_still_surfaces_ascii_content() {
        let mut body = b"AKIA".to_vec();
        body.push(0xff);
        body.extend_from_slice(b"SECRET");
        let output = record("a", "blob", &body);
        let blobs = parse_blob_bodies(&output).unwrap();
        assert!(blobs["a"].contains("AKIA"), "{:?}", blobs["a"]);
        assert!(blobs["a"].contains("SECRET"));
    }

    #[test]
    fn valid_utf8_is_preserved_exactly() {
        assert_eq!(decode_blob("héllo".as_bytes()), "héllo");
    }

    #[test]
    fn a_binary_body_uses_one_character_per_byte() {
        let body = [0x00u8, 0x01, 0xfe];
        assert_eq!(decode_blob(&body).chars().count(), 3);
    }

    #[test]
    fn zero_shas_are_recognized() {
        assert!(is_zero_sha("0000000"));
        assert!(is_zero_sha(&"0".repeat(40)));
        assert!(!is_zero_sha("0001000"));
        assert!(!is_zero_sha(""));
    }

    #[test]
    fn a_short_sha_is_seven_characters() {
        assert_eq!(short_sha("0123456789abcdef"), "0123456");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn the_scanned_modes_exclude_a_gitlink() {
        assert!(is_scanned_blob_mode("100644"));
        assert!(is_scanned_blob_mode("100755"));
        assert!(is_scanned_blob_mode("120000"));
        assert!(!is_scanned_blob_mode(SUBMODULE_MODE));
        assert!(!is_scanned_blob_mode("040000"));
    }
}
