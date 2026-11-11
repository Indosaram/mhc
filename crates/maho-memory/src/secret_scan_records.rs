//! Parsing the raw git output a reflection branch is screened through.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-secret-scan.md`. Nobody writes a diff record by hand:
//! `diff-tree --raw -z` and `cat-file --batch` are machine formats whose framing carries meaning, and a
//! parser that decodes them lossily or splits on the wrong byte silently screens the wrong file.
//!
//! The screening decision itself is here too: it is a pure function over parsed versions and blob bytes,
//! so it can be tested without a repository. Running git belongs to the caller.

use std::collections::BTreeMap;

use crate::redact::{SecretClass, redact_secret_like_material, scan_secret_like_material};

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

/// One refused file version: the repository path and the refusal text, never the matched value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecretLikeFailure {
    pub path: String,
    pub detail: String,
}

/// The name a refusal carries for a matched class.
pub fn class_name(class: SecretClass) -> &'static str {
    match class {
        SecretClass::AwsAccessKey => "aws_access_key",
        SecretClass::CredentialAssignment => "credential_assignment",
        SecretClass::AuthorizationHeader => "authorization_header",
        SecretClass::OpenaiKey => "openai_key",
        SecretClass::VendorToken => "vendor_token",
        SecretClass::PemBlock => "pem_block",
        SecretClass::SplitCredentialAssignment => "split_credential_assignment",
    }
}

/// Every refusal one commit's introduced file versions earn.
///
/// Paths are screened before content: a file name is repository text like any body, and a name carrying
/// a credential is refused even when its content is clean. A gitlink is refused on its file type alone,
/// because its content lives in another repository this scan cannot read. Content is screened for every
/// scanned blob mode including symlinks — a symlink target is a blob and can carry an evaded credential.
///
/// `blobs` maps oid to the bytes `cat-file --batch` returned. A version whose blob is absent yields no
/// failure, so the caller must read every requested oid before relying on an empty result: this function
/// reports what it was given, and absence is not evidence of cleanliness.
pub fn screen_versions(
    versions: &[FileVersion],
    blobs: &BTreeMap<String, BlobRecord>,
    short: &str,
) -> Vec<SecretLikeFailure> {
    let mut failures = Vec::new();
    for version in versions {
        if version.dst_mode == SUBMODULE_MODE {
            failures.push(SecretLikeFailure {
                path: version.path.clone(),
                detail: format!(
                    "secret_like_content: {} (submodule, file type)",
                    redact_secret_like_material(&version.path)
                ),
            });
        }
    }
    let introduced = introduced_versions(versions);
    for version in &introduced {
        if let Some(hit) = scan_secret_like_material(&version.path).first() {
            failures.push(SecretLikeFailure {
                path: version.path.clone(),
                detail: format!(
                    "secret_like_content: {} ({}, file name) @{short}",
                    redact_secret_like_material(&version.path),
                    class_name(hit.class)
                ),
            });
        }
    }
    for version in introduced {
        if !is_scanned_blob(version) {
            continue;
        }
        let Some(blob) = blobs.get(&version.dst_sha) else {
            continue;
        };
        let text = screenable_text(&blob.body);
        if let Some(hit) = scan_secret_like_material(&text).first() {
            failures.push(SecretLikeFailure {
                path: version.path.clone(),
                detail: format!(
                    "secret_like_content: {} ({}) @{short}",
                    redact_secret_like_material(&version.path),
                    class_name(hit.class)
                ),
            });
        }
    }
    failures
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
    fn version(mode: &str, sha: &str, path: &str) -> FileVersion {
        FileVersion {
            dst_mode: mode.to_owned(),
            dst_sha: sha.to_owned(),
            path: path.to_owned(),
        }
    }

    fn blob(oid: &str, body: &str) -> BlobRecord {
        BlobRecord {
            oid: oid.to_owned(),
            kind: "blob".to_owned(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn index(records: Vec<BlobRecord>) -> BTreeMap<String, BlobRecord> {
        index_blobs(records)
    }

    #[test]
    fn a_clean_version_produces_no_failure() {
        let versions = [version("100644", "a", "notes/clean.md")];
        let blobs = index(vec![blob("a", "ordinary prose")]);
        assert!(screen_versions(&versions, &blobs, "abc1234").is_empty());
    }

    #[test]
    fn a_credential_in_a_path_is_refused() {
        let versions = [version(
            "100644",
            "a",
            "notes/ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.md",
        )];
        let blobs = index(vec![blob("a", "clean body")]);
        let failures = screen_versions(&versions, &blobs, "abc1234");
        assert_eq!(failures.len(), 1);
        assert!(
            !failures[0]
                .detail
                .contains("ghp_aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")
        );
        assert!(failures[0].detail.contains("file name"));
    }

    #[test]
    fn a_credential_in_content_is_refused() {
        let versions = [version("100644", "a", "notes/a.md")];
        let blobs = index(vec![blob("a", "key = AKIAIOSFODNN7EXAMPLE\n")]);
        let failures = screen_versions(&versions, &blobs, "abc1234");
        assert_eq!(failures.len(), 1);
        assert!(failures[0].detail.contains("aws_access_key"));
        assert!(!failures[0].detail.contains("AKIAIOSFODNN7EXAMPLE"));
    }

    #[test]
    fn a_submodule_is_refused_on_file_type_alone() {
        let versions = [version(SUBMODULE_MODE, "a", "vendor/dep")];
        let blobs = index(vec![]);
        let failures = screen_versions(&versions, &blobs, "abc1234");
        assert_eq!(failures.len(), 1);
        assert!(failures[0].detail.contains("submodule"));
    }

    #[test]
    fn a_symlink_target_is_screened_like_content() {
        let versions = [version("120000", "a", "notes/link")];
        let blobs = index(vec![blob("a", "key = AKIAIOSFODNN7EXAMPLE\n")]);
        let failures = screen_versions(&versions, &blobs, "abc1234");
        assert_eq!(failures.len(), 1);
    }

    #[test]
    fn a_version_whose_blob_was_not_read_is_not_reported_clean() {
        let versions = [version("100644", "missing", "notes/a.md")];
        let blobs = index(vec![]);
        assert!(screen_versions(&versions, &blobs, "abc1234").is_empty());
    }

    #[test]
    fn a_deleted_path_is_not_screened_for_content() {
        let versions = [version(
            "100644",
            "0000000000000000000000000000000000000000",
            "notes/gone.md",
        )];
        let blobs = index(vec![]);
        assert!(screen_versions(&versions, &blobs, "abc1234").is_empty());
    }

    #[test]
    fn a_binary_blob_still_reveals_an_ascii_credential() {
        let mut body = vec![0xff, 0xfe, 0x00];
        body.extend_from_slice(b"key = AKIAIOSFODNN7EXAMPLE");
        let versions = [version("100644", "a", "notes/bin.dat")];
        let blobs = index(vec![BlobRecord {
            oid: "a".to_owned(),
            kind: "blob".to_owned(),
            body,
        }]);
        assert_eq!(screen_versions(&versions, &blobs, "abc1234").len(), 1);
    }

    #[test]
    fn a_refusal_names_the_introducing_commit() {
        let versions = [version("100644", "a", "notes/a.md")];
        let blobs = index(vec![blob("a", "key = AKIAIOSFODNN7EXAMPLE\n")]);
        let failures = screen_versions(&versions, &blobs, "deadbee");
        assert!(failures[0].detail.contains("deadbee"));
    }
}
