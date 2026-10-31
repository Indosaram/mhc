//! Port of senpi `utils/zip.ts` (pin 903b8336), named `zip_archive` so it does not shadow the
//! `zip` crate. It writes the small, classic deflate ZIP archives used by bug reports. senpi
//! hand-writes the headers over `node:zlib`. This port uses the `zip` crate, which writes the same
//! layout: deflate entries, UTF-8 names, a local-time DOS timestamp and no ZIP64 records for small
//! inputs.

use std::io::{Cursor, Write};
use std::path::Path;

use chrono::{Datelike, Local, Timelike};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

/// senpi `ZipEntry`.
pub struct ZipEntry<'a> {
    pub name: &'a str,
    pub data: &'a [u8],
}

/// senpi `dosDateTime(new Date())`: local time, with the year clamped to the DOS epoch.
fn dos_now() -> DateTime {
    let now = Local::now();
    let year = u16::try_from(now.year().max(1980)).unwrap_or(1980);
    let field = |value: u32| u8::try_from(value).unwrap_or(0);
    DateTime::from_date_and_time(
        year,
        field(now.month()),
        field(now.day()),
        field(now.hour()),
        field(now.minute()),
        field(now.second()),
    )
    .unwrap_or_default()
}

/// senpi `createZipArchive`.
pub fn create_zip_archive(entries: &[ZipEntry<'_>]) -> zip::result::ZipResult<Vec<u8>> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .last_modified_time(dos_now());
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    for entry in entries {
        writer.start_file(entry.name, options)?;
        writer.write_all(entry.data)?;
    }
    Ok(writer.finish()?.into_inner())
}

/// senpi `writeZipArchive`.
pub fn write_zip_archive(file_path: &Path, entries: &[ZipEntry<'_>]) -> std::io::Result<()> {
    let archive = create_zip_archive(entries).map_err(std::io::Error::other)?;
    std::fs::write(file_path, archive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn writes_deflated_entries_that_read_back_unchanged() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("report.zip");
        let log = "line\n".repeat(200);
        write_zip_archive(
            &path,
            &[
                ZipEntry {
                    name: "session.jsonl",
                    data: log.as_bytes(),
                },
                ZipEntry {
                    name: "notes/\u{e9}t\u{e9}.txt",
                    data: b"utf-8 name",
                },
            ],
        )
        .expect("write archive");
        let mut archive =
            zip::ZipArchive::new(std::fs::File::open(&path).expect("open")).expect("read archive");
        assert_eq!(archive.len(), 2);
        let mut first = archive.by_index(0).expect("first entry");
        assert_eq!(first.name(), "session.jsonl");
        assert_eq!(first.compression(), CompressionMethod::Deflated);
        assert!(first.compressed_size() < first.size());
        let mut text = String::new();
        first.read_to_string(&mut text).expect("inflate");
        assert_eq!(text, log);
        drop(first);
        let second = archive.by_index(1).expect("second entry");
        assert_eq!(second.name(), "notes/\u{e9}t\u{e9}.txt");
    }

    #[test]
    fn an_empty_entry_list_is_a_valid_empty_archive() {
        let archive = create_zip_archive(&[]).expect("archive");
        assert_eq!(
            zip::ZipArchive::new(Cursor::new(archive))
                .expect("read")
                .len(),
            0
        );
    }
}
