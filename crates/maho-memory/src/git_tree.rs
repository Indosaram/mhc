//! Parsing `git ls-tree` and `git cat-file --batch` output.
//!
//! Behaviour spec: `docs/cleanroom/specs/memory-git-tree.md`. These two parsers decide which files
//! exist in a revision and what their contents are; a path dropped here is a memory the store reports
//! as absent, and an over-read in `cat-file` would splice one blob's bytes into another.
use std::collections::BTreeMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SizedEntry {
    pub path: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlobEntry {
    pub path: String,
    pub oid: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TreeError {
    TruncatedHeader,
    MalformedHeader {
        header: String,
    },
    TruncatedBody {
        oid: String,
        declared: usize,
        remaining: usize,
    },
}

impl fmt::Display for TreeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TruncatedHeader => write!(f, "cat-file output ended inside a record header"),
            Self::MalformedHeader { header } => write!(f, "malformed cat-file header: {header}"),
            Self::TruncatedBody {
                oid,
                declared,
                remaining,
            } => write!(
                f,
                "blob {oid} declares {declared} bytes but only {remaining} remain"
            ),
        }
    }
}

impl std::error::Error for TreeError {}

fn split_record(record: &str) -> Option<(&str, &str)> {
    let tab = record.find('\t')?;
    Some((&record[..tab], &record[tab + 1..]))
}

pub fn parse_ls_tree_sized(stdout: &str) -> Vec<SizedEntry> {
    stdout
        .split('\0')
        .filter(|record| !record.is_empty())
        .filter_map(|record| {
            let (meta, path) = split_record(record)?;
            if path.is_empty() {
                return None;
            }
            let fields: Vec<&str> = meta.split_whitespace().collect();
            if fields.len() < 4 {
                return None;
            }
            let bytes = fields[3].parse::<u64>().ok()?;
            Some(SizedEntry {
                path: path.to_owned(),
                bytes,
            })
        })
        .collect()
}

pub fn parse_ls_tree_blobs(stdout: &str) -> Vec<BlobEntry> {
    stdout
        .split('\0')
        .filter_map(|record| {
            let (meta, path) = split_record(record)?;
            if path.is_empty() {
                return None;
            }
            let fields: Vec<&str> = meta.split_whitespace().collect();
            if fields.len() < 3 || fields[1] != "blob" {
                return None;
            }
            Some(BlobEntry {
                path: path.to_owned(),
                oid: fields[2].to_owned(),
            })
        })
        .collect()
}

pub fn parse_cat_file_batch(output: &[u8]) -> Result<BTreeMap<String, String>, TreeError> {
    let mut blobs = BTreeMap::new();
    let mut offset = 0usize;
    while offset < output.len() {
        let newline = output[offset..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|relative| offset + relative)
            .ok_or(TreeError::TruncatedHeader)?;
        let header = String::from_utf8_lossy(&output[offset..newline]).into_owned();
        offset = newline + 1;

        let mut parts = header.split(' ');
        let oid = parts.next().unwrap_or_default().to_owned();
        let kind = parts.next().unwrap_or_default();
        if oid.is_empty() || kind.is_empty() {
            return Err(TreeError::MalformedHeader { header });
        }
        // `<oid> missing` is git's answer for an object it cannot find: no size, no body, and the
        // record ends at the header. Consuming it as zero bytes is what keeps the next record framed.
        let Some(size_text) = parts.next() else {
            if kind == "missing" {
                continue;
            }
            return Err(TreeError::MalformedHeader { header });
        };
        let size = size_text
            .parse::<usize>()
            .map_err(|_| TreeError::MalformedHeader {
                header: header.clone(),
            })?;
        let end = offset
            .checked_add(size)
            .filter(|end| *end <= output.len())
            .ok_or_else(|| TreeError::TruncatedBody {
                oid: oid.clone(),
                declared: size,
                remaining: output.len() - offset,
            })?;

        if kind == "blob" {
            blobs.insert(
                oid,
                String::from_utf8_lossy(&output[offset..end]).into_owned(),
            );
        }
        offset = end + 1;
    }
    Ok(blobs)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sized_entry_parses_its_path_and_size() {
        let entries = parse_ls_tree_sized("100644 blob aaa 12\tnotes/a.md\0");
        assert_eq!(
            entries,
            vec![SizedEntry {
                path: "notes/a.md".to_owned(),
                bytes: 12,
            }]
        );
    }

    #[test]
    fn a_submodule_size_marker_is_skipped() {
        assert!(parse_ls_tree_sized("160000 commit aaa -\tvendor/x\0").is_empty());
    }

    #[test]
    fn several_sized_entries_keep_their_order() {
        let entries =
            parse_ls_tree_sized("100644 blob a 3\tone.md\u{0}100644 blob b 3\ttwo.md\u{0}");
        assert_eq!(
            entries
                .iter()
                .map(|entry| entry.path.as_str())
                .collect::<Vec<_>>(),
            ["one.md", "two.md"]
        );
    }

    #[test]
    fn a_path_containing_a_space_survives() {
        let entries = parse_ls_tree_sized("100644 blob aaa 5\tnotes/two words.md\0");
        assert_eq!(entries[0].path, "notes/two words.md");
    }

    #[test]
    fn a_korean_path_survives() {
        let entries = parse_ls_tree_sized("100644 blob aaa 7\tnotes/한국어.md\0");
        assert_eq!(entries[0].path, "notes/한국어.md");
    }

    #[test]
    fn a_record_without_a_tab_is_skipped() {
        assert!(parse_ls_tree_sized("100644 blob aaa 12\0").is_empty());
    }

    #[test]
    fn a_record_with_too_few_metadata_fields_is_skipped() {
        assert!(parse_ls_tree_sized("100644 blob\tnotes/a.md\0").is_empty());
    }

    #[test]
    fn an_empty_path_is_skipped() {
        assert!(parse_ls_tree_sized("100644 blob aaa 12\t\0").is_empty());
    }

    #[test]
    fn an_unparsable_size_is_skipped() {
        assert!(parse_ls_tree_sized("100644 blob aaa notanumber\tnotes/a.md\0").is_empty());
    }

    #[test]
    fn empty_output_has_no_sized_entries() {
        assert!(parse_ls_tree_sized("").is_empty());
    }

    #[test]
    fn a_blob_entry_parses_its_path_and_oid() {
        let entries = parse_ls_tree_blobs("100644 blob deadbeef\tnotes/a.md\0");
        assert_eq!(
            entries,
            vec![BlobEntry {
                path: "notes/a.md".to_owned(),
                oid: "deadbeef".to_owned(),
            }]
        );
    }

    #[test]
    fn a_tree_entry_is_not_a_blob() {
        assert!(parse_ls_tree_blobs("040000 tree deadbeef\tnotes\0").is_empty());
    }

    #[test]
    fn a_blob_entry_with_too_few_metadata_fields_is_skipped() {
        assert!(parse_ls_tree_blobs("100644 blob\tnotes/a.md\0").is_empty());
    }

    #[test]
    fn several_blobs_are_returned_in_order() {
        let entries = parse_ls_tree_blobs("100644 blob a\tone.md\u{0}100644 blob b\ttwo.md\u{0}");
        assert_eq!(entries[1].oid, "b");
        assert_eq!(entries[1].path, "two.md");
    }

    #[test]
    fn a_blob_body_is_read_verbatim() {
        let blobs = parse_cat_file_batch(b"aaa blob 5\nhello\n").expect("blobs");
        assert_eq!(blobs.get("aaa").map(String::as_str), Some("hello"));
    }

    #[test]
    fn a_body_containing_a_newline_is_read_by_declared_size() {
        let blobs = parse_cat_file_batch(b"aaa blob 13\none\ntwo\nthree\n").expect("blobs");
        assert_eq!(
            blobs.get("aaa").map(String::as_str),
            Some("one\ntwo\nthree")
        );
    }

    #[test]
    fn a_body_containing_a_nul_byte_is_read_by_declared_size() {
        let blobs = parse_cat_file_batch(b"aaa blob 3\na\0b\n").expect("blobs");
        assert_eq!(blobs.get("aaa").map(|body| body.as_bytes()[1]), Some(0));
    }

    #[test]
    fn several_blobs_are_read_in_one_pass() {
        let blobs = parse_cat_file_batch(b"aaa blob 3\none\nbbb blob 3\ntwo\n").expect("blobs");
        assert_eq!(blobs.len(), 2);
        assert_eq!(blobs.get("bbb").map(String::as_str), Some("two"));
    }

    #[test]
    fn a_missing_object_yields_no_body() {
        let blobs = parse_cat_file_batch(b"aaa missing\n").expect("blobs");
        assert!(blobs.is_empty());
    }

    #[test]
    fn a_non_blob_object_is_skipped_but_consumed() {
        let blobs = parse_cat_file_batch(b"aaa tree 3\nabc\nbbb blob 3\nxyz\n").expect("blobs");
        assert_eq!(blobs.len(), 1);
        assert_eq!(blobs.get("bbb").map(String::as_str), Some("xyz"));
    }

    #[test]
    fn an_empty_body_is_read_as_empty() {
        let blobs = parse_cat_file_batch(b"aaa blob 0\n\n").expect("blobs");
        assert_eq!(blobs.get("aaa").map(String::as_str), Some(""));
    }

    #[test]
    fn a_header_without_a_trailing_newline_is_an_error() {
        assert_eq!(
            parse_cat_file_batch(b"aaa blob 5").unwrap_err(),
            TreeError::TruncatedHeader
        );
    }

    #[test]
    fn a_body_shorter_than_its_declared_size_is_an_error() {
        let error = parse_cat_file_batch(b"aaa blob 99\nshort\n").unwrap_err();
        assert_eq!(
            error,
            TreeError::TruncatedBody {
                oid: "aaa".to_owned(),
                declared: 99,
                remaining: 6,
            }
        );
    }

    #[test]
    fn a_header_without_a_size_is_an_error() {
        let error = parse_cat_file_batch(b"aaa blob\n").unwrap_err();
        assert!(matches!(error, TreeError::MalformedHeader { .. }));
    }

    #[test]
    fn a_header_with_a_non_numeric_size_is_an_error() {
        let error = parse_cat_file_batch(b"aaa blob big\n").unwrap_err();
        assert!(matches!(error, TreeError::MalformedHeader { .. }));
    }

    #[test]
    fn an_oversized_size_does_not_overflow() {
        let error = parse_cat_file_batch(b"aaa blob 18446744073709551615\n").unwrap_err();
        assert!(matches!(
            error,
            TreeError::MalformedHeader { .. } | TreeError::TruncatedBody { .. }
        ));
    }

    #[test]
    fn an_empty_output_has_no_blobs() {
        assert!(parse_cat_file_batch(b"").expect("blobs").is_empty());
    }

    #[test]
    fn a_korean_body_survives() {
        let mut record = b"aaa blob 9\n".to_vec();
        record.extend_from_slice("한국어".as_bytes());
        record.push(b'\n');
        let blobs = parse_cat_file_batch(&record).expect("blobs");
        assert_eq!(blobs.get("aaa").map(String::as_str), Some("한국어"));
    }
}
