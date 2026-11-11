//! Parsing the raw git output a reflection branch is screened through.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-secret-scan.md`. Nobody writes a diff record by hand:
//! `diff-tree --raw -z` and `cat-file --batch` are machine formats whose framing carries meaning, and a
//! parser that decodes them lossily or splits on the wrong byte silently screens the wrong file.

/// Blob modes whose content is screened. A gitlink is refused on its file type alone.
pub const SCANNED_BLOB_MODES: [&str; 3] = ["100644", "100755", "120000"];

pub const SUBMODULE_MODE: &str = "160000";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileVersion {
    pub dst_mode: String,
    pub dst_sha: String,
    pub path: String,
}

/// Parse `git diff-tree --raw -z` output: `:<src_mode> <dst_mode> <src_sha> <dst_sha> <status>\0<path>\0`.
///
/// The `-z` terminator is why this cannot split on whitespace or newlines: a path may contain either.
/// Records whose path is empty or that carry fewer than five header fields are skipped rather than
/// treated as errors, because `--diff-filter=AMT` can still emit framing for a pair the filter removed.
pub fn parse_raw_diff(output: &str) -> Vec<FileVersion> {
    let parts: Vec<&str> = output.split('\u{0}').collect();
    let mut records = Vec::new();
    let mut index = 0;
    while index + 1 < parts.len() {
        let header = parts[index];
        let path = parts[index + 1];
        index += 2;
        if path.is_empty() || !header.starts_with(':') {
            continue;
        }
        let fields: Vec<&str> = header[1..].split(' ').collect();
        if fields.len() < 5 {
            continue;
        }
        let (Some(dst_mode), Some(dst_sha)) = (fields.get(1), fields.get(3)) else {
            continue;
        };
        records.push(FileVersion {
            dst_mode: (*dst_mode).to_owned(),
            dst_sha: (*dst_sha).to_owned(),
            path: path.to_owned(),
        });
    }
    records
}

pub fn is_scanned_blob(record: &FileVersion) -> bool {
    SCANNED_BLOB_MODES.contains(&record.dst_mode.as_str())
}

pub fn is_submodule(record: &FileVersion) -> bool {
    record.dst_mode == SUBMODULE_MODE
}

pub fn is_zero_sha(sha: &str) -> bool {
    !sha.is_empty() && sha.bytes().all(|byte| byte == b'0')
}

pub fn introduced_versions(records: &[FileVersion]) -> Vec<&FileVersion> {
    records
        .iter()
        .filter(|record| !is_zero_sha(&record.dst_sha))
        .collect()
}

pub fn short_sha(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRecord {
    pub oid: String,
    pub kind: String,
    pub body: Vec<u8>,
}

/// Walk `cat-file --batch` output on raw bytes.
///
/// Each record is `<oid> <type> <size>\n`, then exactly `size` bytes, then one newline. The walk must
/// stay on bytes: decoding to text first would let a multi-byte sequence inside a blob shift every
/// following record, so a credential in the second file could be missed. A blob whose bytes are not
/// UTF-8 is still returned here; the caller decides how to screen it.
pub fn parse_blob_records(output: &[u8]) -> Result<Vec<BlobRecord>, String> {
    let mut records = Vec::new();
    let mut offset = 0usize;
    while offset < output.len() {
        let Some(newline_offset) = output[offset..].iter().position(|byte| *byte == b'\n') else {
            return Err("git cat-file --batch output ended inside a record header".to_owned());
        };
        let newline = offset + newline_offset;
        let header = String::from_utf8_lossy(&output[offset..newline]).into_owned();
        offset = newline + 1;

        let mut fields = header.split(' ');
        let oid = fields.next().unwrap_or_default();
        let kind = fields.next().unwrap_or_default();
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
        let body = output[offset..offset + size].to_vec();
        offset += size;
        if output.get(offset) == Some(&b'\n') {
            offset += 1;
        }
        records.push(BlobRecord {
            oid: oid.to_owned(),
            kind: kind.to_owned(),
            body,
        });
    }
    Ok(records)
}

/// UTF-8 when valid, otherwise Latin-1 (one character per byte).
///
/// Screening a non-UTF-8 blob with `from_utf8` lossily would replace invalid bytes with U+FFFD and let a binary
/// carry an ASCII credential past the scanner. Latin-1 keeps every byte distinct.
pub fn screenable_text(body: &[u8]) -> String {
    match std::str::from_utf8(body) {
        Ok(text) => text.to_owned(),
        Err(_) => body.iter().map(|byte| *byte as char).collect(),
    }
}

pub fn index_blobs(records: Vec<BlobRecord>) -> std::collections::BTreeMap<String, BlobRecord> {
    let mut index = std::collections::BTreeMap::new();
    for record in records {
        index.entry(record.oid.clone()).or_insert(record);
    }
    index
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str =
        ":100644 100644 aaa bbb M\u{0}notes/a.md\u{0}:000000 120000 000 ccc A\u{0}link\u{0}";

    #[test]
    fn valid_utf8_screens_as_itself() {
        assert_eq!(screenable_text("héllo".as_bytes()), "héllo");
    }

    #[test]
    fn invalid_utf8_keeps_every_byte_distinct() {
        let text = screenable_text(&[0xff, b'A', 0x00]);
        assert_eq!(text.chars().count(), 3);
        assert!(text.contains('A'));
    }

    #[test]
    fn a_binary_cannot_hide_an_ascii_credential() {
        let mut body = b"AKIA".to_vec();
        body.push(0xff);
        body.extend_from_slice(b"SECRET");
        let text = screenable_text(&body);
        assert!(text.contains("AKIA"), "{text:?}");
        assert!(text.contains("SECRET"), "{text:?}");
    }

    #[test]
    fn a_raw_diff_record_parses_its_destination() {
        let records = parse_raw_diff(RAW);
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].path, "notes/a.md");
        assert_eq!(records[0].dst_mode, "100644");
        assert_eq!(records[0].dst_sha, "bbb");
        assert_eq!(records[1].dst_mode, "120000");
        assert_eq!(records[1].path, "link");
    }

    #[test]
    fn a_path_containing_a_space_stays_one_record() {
        let records = parse_raw_diff(":100644 100644 a b M\u{0}a note with spaces.md\u{0}");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, "a note with spaces.md");
    }

    #[test]
    fn a_path_containing_a_newline_stays_one_record() {
        let records = parse_raw_diff(":100644 100644 a b M\u{0}odd\nname.md\u{0}");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].path, "odd\nname.md");
    }

    #[test]
    fn a_malformed_record_is_skipped_not_panicked_on() {
        assert!(parse_raw_diff(":100644 100644 a\u{0}p.md\u{0}").is_empty());
        assert!(parse_raw_diff("not a header\u{0}p.md\u{0}").is_empty());
        assert!(parse_raw_diff("").is_empty());
    }

    #[test]
    fn an_empty_path_is_skipped() {
        assert!(parse_raw_diff(":100644 100644 a b M\u{0}\u{0}").is_empty());
    }

    #[test]
    fn blob_modes_are_screened_and_a_gitlink_is_not() {
        let blob = FileVersion {
            dst_mode: "100644".to_owned(),
            dst_sha: "a".to_owned(),
            path: "x".to_owned(),
        };
        assert!(is_scanned_blob(&blob));
        let symlink = FileVersion {
            dst_mode: "120000".to_owned(),
            ..blob.clone()
        };
        assert!(is_scanned_blob(&symlink), "a symlink target is a blob");
        let submodule = FileVersion {
            dst_mode: SUBMODULE_MODE.to_owned(),
            ..blob.clone()
        };
        assert!(!is_scanned_blob(&submodule));
        assert!(is_submodule(&submodule));
    }

    #[test]
    fn a_zero_sha_is_a_deletion_not_an_introduction() {
        assert!(is_zero_sha("0000000"));
        assert!(!is_zero_sha("0000001"));
        assert!(!is_zero_sha(""));
        let records = vec![
            FileVersion {
                dst_mode: "100644".to_owned(),
                dst_sha: "000".to_owned(),
                path: "gone.md".to_owned(),
            },
            FileVersion {
                dst_mode: "100644".to_owned(),
                dst_sha: "abc".to_owned(),
                path: "kept.md".to_owned(),
            },
        ];
        let introduced = introduced_versions(&records);
        assert_eq!(introduced.len(), 1);
        assert_eq!(introduced[0].path, "kept.md");
    }

    #[test]
    fn a_short_sha_keeps_seven_characters() {
        assert_eq!(short_sha("0123456789abcdef"), "0123456");
        assert_eq!(short_sha("abc"), "abc");
    }

    #[test]
    fn a_batch_blob_parses_its_body() {
        let output = b"oid1 blob 5\nhello\n";
        let records = parse_blob_records(output).expect("parses");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].oid, "oid1");
        assert_eq!(records[0].kind, "blob");
        assert_eq!(records[0].body, b"hello");
    }

    #[test]
    fn two_batch_blobs_parse_in_order() {
        let output = b"a blob 3\none\nb blob 3\ntwo\n";
        let records = parse_blob_records(output).expect("parses");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].body, b"one");
        assert_eq!(records[1].body, b"two");
    }

    #[test]
    fn a_body_containing_a_newline_is_not_a_record_boundary() {
        let output = b"a blob 12\nline1\nline2\n\nb blob 1\nz\n";
        let records = parse_blob_records(output).expect("parses");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].body, b"line1\nline2\n");
        assert_eq!(records[1].body, b"z");
    }

    #[test]
    fn a_multibyte_body_does_not_shift_the_next_record() {
        let body = "한국어".as_bytes();
        let mut output = format!("a blob {}\n", body.len()).into_bytes();
        output.extend_from_slice(body);
        output.push(b'\n');
        output.extend_from_slice(b"b blob 2\nok\n");
        let records = parse_blob_records(&output).expect("parses");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].body, body);
        assert_eq!(records[1].oid, "b");
        assert_eq!(records[1].body, b"ok");
    }

    #[test]
    fn a_non_utf8_body_survives_the_walk() {
        let mut output = b"a blob 3\n".to_vec();
        output.extend_from_slice(&[0xff, 0xfe, 0x41]);
        output.push(b'\n');
        output.extend_from_slice(b"b blob 1\nz\n");
        let records = parse_blob_records(&output).expect("parses");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].body, vec![0xff, 0xfe, 0x41]);
        assert_eq!(records[1].oid, "b");
    }

    #[test]
    fn a_truncated_body_is_an_error_not_a_short_read() {
        let output = b"a blob 99\nshort\n";
        assert!(parse_blob_records(output).is_err());
    }

    #[test]
    fn a_header_without_a_size_terminator_is_an_error() {
        assert!(parse_blob_records(b"a blob 3\none").is_ok());
        assert!(parse_blob_records(b"a blob").is_err());
    }

    #[test]
    fn a_missing_object_header_continues_the_walk() {
        let output = b"missing blob\nb blob 1\nz\n";
        let records = parse_blob_records(output).expect("parses");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].oid, "b");
    }

    #[test]
    fn indexing_keeps_one_version_per_oid() {
        let records = vec![
            BlobRecord {
                oid: "a".to_owned(),
                kind: "blob".to_owned(),
                body: b"first".to_vec(),
            },
            BlobRecord {
                oid: "a".to_owned(),
                kind: "blob".to_owned(),
                body: b"second".to_vec(),
            },
        ];
        let index = index_blobs(records);
        assert_eq!(index.len(), 1);
        assert_eq!(index["a"].body, b"first");
    }
}
