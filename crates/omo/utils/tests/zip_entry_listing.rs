//! Translated from zip-entry-listing/powershell-zip-entry-listing,
//! zip-entry-listing/tar-zip-entry-listing, and zip-entry-listing/zipinfo-zip-entry-listing tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use pretty_assertions::assert_eq;
use tempfile::TempDir;
use utils::*;

static TAR_LOG_MUTEX: Mutex<()> = Mutex::new(());

fn tempdir() -> TempDir {
    TempDir::new().unwrap_or_else(|error| panic!("{error}"))
}

fn text(path: &Path) -> String {
    path.display().to_string()
}

fn tar_line(name: &str) -> String {
    format!("-rw-r--r-- 1 user group 123 Jan 01 12:34 {name}")
}

fn capture_log_lines() -> Arc<Mutex<Vec<String>>> {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lines);
    configure_shared_subunit_logger(Some(Arc::new(
        move |message: &str, data: Option<&serde_json::Value>| {
            let line = (message == "warning: unparsed tar listing line")
                .then(|| {
                    data.and_then(|data| data.get("line"))
                        .and_then(serde_json::Value::as_str)
                })
                .flatten();
            if let Some(line) = line {
                sink.lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(line.to_string());
            }
        },
    )));
    lines
}

fn warned(lines: &Arc<Mutex<Vec<String>>>) -> Vec<String> {
    lines
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

fn fixture_zip(entry_name: &str, bytes: &[u8]) -> Vec<u8> {
    let name = entry_name.as_bytes();
    let size = u32::try_from(bytes.len()).unwrap_or_default().to_le_bytes();
    let name_len = u16::try_from(name.len()).unwrap_or_default().to_le_bytes();
    let mut local = Vec::new();
    local.extend_from_slice(&0x0403_4b50_u32.to_le_bytes());
    local.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    local.extend_from_slice(&[0, 0, 0, 0]);
    local.extend_from_slice(&size);
    local.extend_from_slice(&size);
    local.extend_from_slice(&name_len);
    local.extend_from_slice(&[0, 0]);
    local.extend_from_slice(name);
    let mut central = Vec::new();
    central.extend_from_slice(&0x0201_4b50_u32.to_le_bytes());
    central.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    central.extend_from_slice(&[0, 0, 0, 0]);
    central.extend_from_slice(&size);
    central.extend_from_slice(&size);
    central.extend_from_slice(&name_len);
    central.extend_from_slice(&[0; 12]);
    central.extend_from_slice(&[0, 0, 0, 0]);
    central.extend_from_slice(name);
    let central_offset = u32::try_from(local.len() + bytes.len()).unwrap_or_default();
    let mut zip = local;
    zip.extend_from_slice(bytes);
    let central_len = u32::try_from(central.len()).unwrap_or_default();
    zip.extend_from_slice(&central);
    zip.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    zip.extend_from_slice(&[0, 0, 0, 0, 1, 0, 1, 0]);
    zip.extend_from_slice(&central_len.to_le_bytes());
    zip.extend_from_slice(&central_offset.to_le_bytes());
    zip.extend_from_slice(&[0, 0]);
    zip
}

#[test]
fn powershell_line_keeps_tab_path_for_traversal_check() {
    let line =
        serde_json::json!({"type": "file", "name": "safe.txt\t../../escape.txt", "target": ""})
            .to_string();
    let parsed = parse_power_shell_zip_entry_line(&line).ok().flatten();
    let expected = ArchiveEntry {
        path: "safe.txt\t../../escape.txt".to_string(),
        entry_type: ArchiveEntryType::File,
        link_path: None,
    };
    assert_eq!(parsed.as_ref(), Some(&expected));
    let error = validate_archive_entries(&[expected], "/tmp/archive-root")
        .err()
        .map(|error| error.message.to_lowercase());
    assert!(error.is_some_and(|message| message.contains("path traversal")));
}

#[test]
fn tar_listing_fails_closed_on_one_unparsed_line() {
    let _lock = TAR_LOG_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lines = capture_log_lines();
    let output = [
        tar_line("file-1.txt"),
        tar_line("file-2.txt"),
        "unparsed listing line".to_string(),
    ]
    .join("\n");
    let error = parse_tar_listing_output(&output)
        .err()
        .map(|error| error.message)
        .unwrap_or_default();
    assert!(error.to_lowercase().contains("could not be parsed"));
    assert!(warned(&lines).contains(&"unparsed listing line".to_string()));
    configure_shared_subunit_logger(None);
}

#[test]
fn tar_listing_fails_closed_with_counts() {
    let _lock = TAR_LOG_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lines = capture_log_lines();
    let mut output: Vec<String> = (1..=8)
        .map(|n| tar_line(&format!("file-{n}.txt")))
        .collect();
    output.extend([
        "unparsed listing line 1".to_string(),
        "unparsed listing line 2".to_string(),
    ]);
    let error = parse_tar_listing_output(&output.join("\n"))
        .err()
        .map(|error| error.message)
        .unwrap_or_default();
    assert!(error.contains("2/10") && error.contains("could not be parsed"));
    let logged = warned(&lines);
    assert!(
        logged.contains(&"unparsed listing line 1".to_string())
            && logged.contains(&"unparsed listing line 2".to_string())
    );
    configure_shared_subunit_logger(None);
}

#[test]
fn tar_listing_rejects_fully_unparsed_output() {
    let _lock = TAR_LOG_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let lines = capture_log_lines();
    let error = parse_tar_listing_output("unknown format 1\nunknown format 2")
        .err()
        .map(|error| error.message)
        .unwrap_or_default();
    assert!(error.contains("could not be parsed"));
    assert_eq!(warned(&lines), vec!["unknown format 1", "unknown format 2"]);
    configure_shared_subunit_logger(None);
}

#[test]
fn zipinfo_line_preserves_trailing_whitespace() {
    let line =
        "?rw-------  2.0 unx        1 b-        1 stor 26-Apr-03 18:33   trailing-space.txt ";
    assert_eq!(
        parse_zip_info_listed_entry(line),
        Some(ArchiveEntry {
            path: "trailing-space.txt ".to_string(),
            entry_type: ArchiveEntryType::File,
            link_path: None
        })
    );
}

#[test]
fn zip_listing_with_python_reads_real_archive() {
    if !is_python_zip_listing_available() {
        return;
    }
    let dir = tempdir();
    let archive: PathBuf = dir.path().join("fixture.zip");
    fs::write(&archive, fixture_zip("bin/tool", b"payload")).unwrap_or_default();
    let entries =
        list_zip_entries_with_python(&text(&archive)).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        entries,
        vec![ArchiveEntry {
            path: "bin/tool".to_string(),
            entry_type: ArchiveEntryType::File,
            link_path: None
        }]
    );
}
