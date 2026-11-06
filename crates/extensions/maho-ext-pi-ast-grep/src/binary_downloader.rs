use std::path::{Path, PathBuf};

/// Upstream `getCachedBinaryPath(cacheDir, binaryName)`: `${cacheDir}/${binaryName}` when it exists.
pub fn get_cached_binary_path(cache_dir: &Path, binary_name: &str) -> Option<PathBuf> {
    let path = cache_dir.join(binary_name);
    path.exists().then_some(path)
}
/// Upstream `ensureCacheDir(cacheDir)`.
pub fn ensure_cache_dir(cache_dir: &Path) -> std::io::Result<()> {
    if !cache_dir.exists() {
        std::fs::create_dir_all(cache_dir)?;
    }
    Ok(())
}

pub async fn download_archive(
    url: &str,
    archive: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut response = reqwest::Client::new().get(url).send().await?;
    if !response.status().is_success() {
        return Err(crate::errors::AstGrepDownloadError::new(format!(
            "HTTP {}: {}",
            response.status().as_u16(),
            response.status().canonical_reason().unwrap_or_default()
        ))
        .into());
    }
    let mut file = std::fs::File::create(archive)?;
    while let Some(chunk) = response.chunk().await? {
        std::io::Write::write_all(&mut file, &chunk)?;
    }
    Ok(())
}
pub fn extract_zip_archive(
    archive: &Path,
    destination: &Path,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    std::fs::create_dir_all(destination)?;
    let destination = destination.canonicalize()?;
    let mut archive = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let name = entry.name()?.replace('\\', "/");
        if name.starts_with('/')
            || (name.as_bytes().get(1) == Some(&b':') && name.as_bytes()[0].is_ascii_alphabetic())
        {
            return Err(std::io::Error::other(format!("absolute path: {name}")).into());
        }
        if name.split('/').any(|part| part == "..") {
            return Err(std::io::Error::other(format!("invalid relative path: {name}")).into());
        }
        if name.starts_with("__MACOSX/") {
            continue;
        }
        let output = destination.join(&name);
        let parent = output
            .parent()
            .ok_or_else(|| std::io::Error::other("archive entry has no parent"))?;
        std::fs::create_dir_all(parent)?;
        let canonical_parent = parent.canonicalize()?;
        if !canonical_parent.starts_with(&destination) {
            return Err(std::io::Error::other(format!(
                "Out of bound path \"{}\" found while processing file {name}",
                canonical_parent.display()
            ))
            .into());
        }
        // extract-zip: `mode = (externalFileAttributes >> 16) & 0xFFFF`; a directory is IFDIR, a
        // trailing `/`, or the Windows form `versionMadeBy >> 8 === 0 && externalFileAttributes === 16`.
        let (external_attributes, made_by_dos) = {
            use zip::read::HasZipMetadata;
            let metadata = entry.get_metadata();
            (metadata.external_attributes, metadata.system as u8 == 0)
        };
        let entry_mode = (external_attributes >> 16) & 0xFFFF;
        let directory = entry_mode & 0o170000 == 0o040000
            || name.ends_with('/')
            || (made_by_dos && external_attributes == 16);
        let mode = if entry_mode == 0 {
            if directory { 0o755 } else { 0o644 }
        } else {
            entry_mode
        };
        if directory {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode(mode & 0o777)
                    .create(&output)?;
            }
            #[cfg(not(unix))]
            std::fs::create_dir_all(&output)?;
        } else if mode & 0o170000 == 0o120000 {
            let mut target = String::new();
            std::io::Read::read_to_string(&mut entry, &mut target)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(target, &output)?;
            #[cfg(windows)]
            std::os::windows::fs::symlink_file(target, &output)?;
        } else {
            let mut options = std::fs::OpenOptions::new();
            options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(mode & 0o777);
            }
            std::io::copy(&mut entry, &mut options.open(&output)?)?;
        }
    }
    Ok(())
}
pub fn cleanup_archive(archive: &Path) -> std::io::Result<()> {
    if archive.exists() {
        std::fs::remove_file(archive)?;
    }
    Ok(())
}
pub fn ensure_executable(binary: &Path) -> std::io::Result<()> {
    #[cfg(not(unix))]
    let _ = binary;
    #[cfg(unix)]
    if binary.exists() {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(binary, std::fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn http_failure_preserves_download_error_type() {
        use std::io::{BufRead, Write};
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind download fixture");
        let address = listener.local_addr().expect("fixture address");
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("download request");
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .expect("bound request");
            let mut reader =
                std::io::BufReader::new(stream.try_clone().expect("clone HTTP stream"));
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).expect("read HTTP header") > 0);
                if line == "\r\n" {
                    break;
                }
            }
            stream
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .expect("respond HTTP failure");
        });
        let fixture = tempfile::tempdir().expect("download output fixture");
        let result = download_archive(
            &format!("http://{address}/missing.zip"),
            &fixture.path().join("missing.zip"),
        )
        .await;
        peer.join().expect("join HTTP peer");
        let error = result.expect_err("expected HTTP error");
        let error = error
            .downcast_ref::<crate::errors::AstGrepDownloadError>()
            .expect("typed HTTP error");
        assert_eq!(error.message, "HTTP 404: Not Found");
    }
    #[test]
    fn cached_binary_path_and_cache_dir_helpers() {
        let fixture = tempfile::tempdir().expect("cache fixture");
        let cache = fixture.path().join("nested/bin");
        assert_eq!(get_cached_binary_path(&cache, "sg"), None);
        ensure_cache_dir(&cache).expect("create cache dir");
        ensure_cache_dir(&cache).expect("existing cache dir");
        std::fs::write(cache.join("sg"), "binary").expect("write cached binary");
        assert_eq!(get_cached_binary_path(&cache, "sg"), Some(cache.join("sg")));
    }
    #[cfg(unix)]
    #[test]
    fn unix_entry_mode_is_preserved() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("archive.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        writer
            .start_file(
                "bin/sg",
                zip::write::SimpleFileOptions::default().unix_permissions(0o755),
            )
            .unwrap();
        writer.write_all(b"data").unwrap();
        writer.finish().unwrap();
        let output = fixture.path().join("output");
        extract_zip_archive(&path, &output).unwrap();
        assert_eq!(
            std::fs::metadata(output.join("bin/sg"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    /// Writes a one-entry zip whose central directory claims MS-DOS (`versionMadeBy >> 8 === 0`)
    /// with the given `externalFileAttributes`.
    fn dos_entry_archive(dir: &Path, name: &str, external_attributes: u32) -> PathBuf {
        use std::io::Write;
        let path = dir.join(format!("{name}.zip"));
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        writer
            .start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"data").unwrap();
        writer.finish().unwrap();
        let mut bytes = std::fs::read(&path).unwrap();
        let central = bytes
            .windows(4)
            .position(|window| window == [0x50, 0x4b, 0x01, 0x02])
            .expect("central directory header");
        bytes[central + 5] = 0;
        bytes[central + 38..central + 42].copy_from_slice(&external_attributes.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        path
    }
    /// extract-zip: a DOS-made entry with `externalFileAttributes === 16` is a directory even
    /// without a trailing `/`.
    #[test]
    fn dos_directory_attribute_extracts_as_directory() {
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("output");
        extract_zip_archive(&dos_entry_archive(fixture.path(), "dosdir", 16), &output).unwrap();
        assert!(output.join("dosdir").is_dir());
    }
    /// extract-zip only honours the exact value 16 and otherwise reads the mode from the high
    /// 16 bits (0 for DOS entries): a read-only directory-bit entry (0x11) is a 0644 file, whereas
    /// zip's own DOS mapping (`unix_mode()`) would make it a directory.
    #[cfg(unix)]
    #[test]
    fn dos_attributes_other_than_16_extract_as_files() {
        use std::os::unix::fs::PermissionsExt;
        let fixture = tempfile::tempdir().unwrap();
        let output = fixture.path().join("output");
        let archive = dos_entry_archive(fixture.path(), "readonly_dir_bits", 0x11);
        extract_zip_archive(&archive, &output).unwrap();
        let metadata = std::fs::symlink_metadata(output.join("readonly_dir_bits")).unwrap();
        assert!(metadata.is_file());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
    }
    #[test]
    fn extracts_binary_and_removes_archive() {
        use std::io::Write;
        let fixture = tempfile::tempdir().expect("create archive fixture");
        let path = fixture.path().join("binary.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).expect("create zip"));
        writer
            .start_file("sg", zip::write::SimpleFileOptions::default())
            .expect("start binary entry");
        writer
            .write_all(b"fixture binary")
            .expect("write binary entry");
        writer.finish().expect("finish zip");
        let destination = fixture.path().join("bin");
        extract_zip_archive(&path, &destination).expect("extract archive");
        assert_eq!(
            std::fs::read(destination.join("sg")).expect("read extracted binary"),
            b"fixture binary"
        );
        ensure_executable(&destination.join("sg")).expect("mark executable");
        cleanup_archive(&path).expect("remove archive");
        assert!(!path.exists());
    }

    #[test]
    fn traversal_is_rejected_with_reference_error() {
        use std::io::Write;
        for name in ["../escaped", "/absolute", "C:/absolute"] {
            let fixture = tempfile::tempdir().expect("archive fixture");
            let path = fixture.path().join("archive.zip");
            let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"data").unwrap();
            writer.finish().unwrap();
            let error = extract_zip_archive(&path, &fixture.path().join("output")).unwrap_err();
            let prefix = if name.starts_with("..") {
                "invalid relative path"
            } else {
                "absolute path"
            };
            assert_eq!(error.to_string(), format!("{prefix}: {name}"));
            assert!(!fixture.path().join("escaped").exists());
        }
    }

    #[test]
    fn macos_metadata_is_omitted_and_backslashes_are_normalized() {
        use std::io::Write;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("archive.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        for name in ["__MACOSX/metadata", "bin\\sg"] {
            writer
                .start_file(name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(b"data").unwrap();
        }
        writer.finish().unwrap();
        let output = fixture.path().join("output");
        extract_zip_archive(&path, &output).unwrap();
        assert!(!output.join("__MACOSX").exists());
        assert_eq!(std::fs::read(output.join("bin/sg")).unwrap(), b"data");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_parent_escape_is_rejected() {
        use std::io::Write;
        let fixture = tempfile::tempdir().unwrap();
        let outside_dir = tempfile::tempdir().unwrap();
        let outside = outside_dir.path().canonicalize().unwrap();
        let path = fixture.path().join("archive.zip");
        let output = fixture.path().join("output");
        std::fs::create_dir(&output).unwrap();
        std::os::unix::fs::symlink(&outside, output.join("link")).unwrap();
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).unwrap());
        writer
            .start_file("link/escaped", zip::write::SimpleFileOptions::default())
            .unwrap();
        writer.write_all(b"data").unwrap();
        writer.finish().unwrap();
        assert_eq!(
            extract_zip_archive(&path, &output).unwrap_err().to_string(),
            format!(
                "Out of bound path \"{}\" found while processing file link/escaped",
                outside.display()
            )
        );
        assert!(!outside.join("escaped").exists());
    }
}
