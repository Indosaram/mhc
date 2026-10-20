use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

const MAX_EXPANDED: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 100_000;

fn destination(root: &Path, name: &str, seen: &mut BTreeSet<PathBuf>) -> Result<PathBuf, String> {
    let relative = super::manifest::relative_path(name.trim_end_matches('/'))?;
    if seen.len() >= MAX_ENTRIES || !seen.insert(relative.clone()) {
        return Err(format!("duplicate or excessive archive entry: {name}"));
    }
    Ok(root.join(relative))
}

fn write_entry(path: &Path, size: u64, reader: impl Read, total: &mut u64) -> Result<(), String> {
    *total = total.checked_add(size).ok_or("archive size overflow")?;
    if *total > MAX_EXPANDED { return Err("expanded archive exceeds size limit".into()); }
    let parent = path.parent().ok_or("archive entry has no parent")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut output = OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| e.to_string())?;
    let written = io::copy(&mut reader.take(size + 1), &mut output).map_err(|e| e.to_string())?;
    if written != size { return Err(format!("archive entry length mismatch: {}", path.display())); }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777)).map_err(|e| e.to_string())?;
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
    Ok(())
}

pub fn extract(archive: &Path, root: &Path, zipped: bool) -> Result<PathBuf, String> {
    fs::create_dir(root).map_err(|e| e.to_string())?;
    let mut seen = BTreeSet::new();
    let mut total = 0;
    let file = File::open(archive).map_err(|e| e.to_string())?;
    if zipped {
        let mut zip = zip::ZipArchive::new(file).map_err(|e| e.to_string())?;
        if zip.len() > MAX_ENTRIES { return Err("too many archive entries".into()); }
        for index in 0..zip.len() {
            let mut entry = zip.by_index(index).map_err(|e| e.to_string())?;
            let path = destination(root, entry.name(), &mut seen)?;
            let mode = entry.unix_mode().unwrap_or(if entry.is_dir() { 0o40755 } else { 0o100644 });
            let kind = mode & 0o170000;
            if entry.is_dir() && (kind == 0 || kind == 0o40000) {
                fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            } else if !entry.is_dir() && (kind == 0 || kind == 0o100000) {
                write_entry(&path, entry.size(), &mut entry, &mut total)?;
                set_mode(&path, mode)?;
            } else { return Err("archive contains link or special file".into()); }
        }
    } else {
        let decoder = flate2::read::GzDecoder::new(file);
        let mut tar = tar::Archive::new(decoder);
        for entry in tar.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            let name = entry.path().map_err(|e| e.to_string())?.to_str().ok_or("non-UTF8 archive path")?.to_owned();
            let path = destination(root, &name, &mut seen)?;
            let kind = entry.header().entry_type();
            if kind.is_dir() {
                fs::create_dir_all(&path).map_err(|e| e.to_string())?;
            } else if kind.is_file() {
                let size = entry.size();
                let mode = entry.header().mode().map_err(|e| e.to_string())?;
                write_entry(&path, size, &mut entry, &mut total)?;
                set_mode(&path, mode)?;
            } else { return Err("archive contains link or special file".into()); }
        }
    }
    let entries = fs::read_dir(root).map_err(|e| e.to_string())?
        .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    if entries.len() != 1 || !entries[0].file_type().map_err(|e| e.to_string())?.is_dir() {
        return Err("archive must contain exactly one package directory".into());
    }
    Ok(entries[0].path())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn zip_extracts_regular_files_but_rejects_escape() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let archive = directory.path().join("package.zip");
        let mut writer = zip::ZipWriter::new(File::create(&archive)?);
        writer.start_file("package/mhc", zip::write::SimpleFileOptions::default())?;
        writer.write_all(b"candidate")?;
        writer.finish()?;
        let root = extract(&archive, &directory.path().join("extracted"), true)?;
        assert_eq!(fs::read(root.join("mhc"))?, b"candidate");

        let malicious = directory.path().join("escape.zip");
        let mut writer = zip::ZipWriter::new(File::create(&malicious)?);
        writer.start_file("../outside", zip::write::SimpleFileOptions::default())?;
        writer.write_all(b"escape")?;
        writer.finish()?;
        assert!(extract(&malicious, &directory.path().join("rejected"), true).is_err());
        assert!(!directory.path().join("outside").exists());
        Ok(())
    }

    #[test]
    fn tar_rejects_links_before_materializing_them() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let archive = directory.path().join("package.tar.gz");
        let encoder = flate2::write::GzEncoder::new(File::create(&archive)?, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        builder.append_link(&mut header, "package/link", "../../outside")?;
        builder.into_inner()?.finish()?;
        let destination = directory.path().join("extracted");
        assert!(extract(&archive, &destination, false).is_err());
        assert!(fs::symlink_metadata(destination.join("package/link")).is_err());
        Ok(())
    }

    #[test]
    fn oversized_entries_are_rejected_before_writing() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("oversized");
        assert!(write_entry(&path, MAX_EXPANDED + 1, io::empty(), &mut 0).is_err());
        assert!(!path.exists());
        Ok(())
    }
}
