use std::path::Path;

pub async fn download_archive(url: &str, archive: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut response = reqwest::Client::new().get(url).send().await?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}: {}", response.status().as_u16(), response.status().canonical_reason().unwrap_or_default()).into());
    }
    let mut file = std::fs::File::create(archive)?;
    while let Some(chunk) = response.chunk().await? { std::io::Write::write_all(&mut file, &chunk)?; }
    Ok(())
}
pub fn extract_zip_archive(archive: &Path, destination: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(archive)?)?;
    archive.extract(destination)?;
    Ok(())
}
pub fn cleanup_archive(archive: &Path) -> std::io::Result<()> {
    if archive.exists() { std::fs::remove_file(archive)?; }
    Ok(())
}
pub fn ensure_executable(binary: &Path) -> std::io::Result<()> {
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
    #[test]
    fn extracts_binary_and_removes_archive() {
        use std::io::Write;
        let fixture = tempfile::tempdir().expect("create archive fixture");
        let path = fixture.path().join("binary.zip");
        let mut writer = zip::ZipWriter::new(std::fs::File::create(&path).expect("create zip"));
        writer.start_file("sg", zip::write::SimpleFileOptions::default()).expect("start binary entry");
        writer.write_all(b"fixture binary").expect("write binary entry");
        writer.finish().expect("finish zip");
        let destination = fixture.path().join("bin");
        extract_zip_archive(&path, &destination).expect("extract archive");
        assert_eq!(std::fs::read(destination.join("sg")).expect("read extracted binary"), b"fixture binary");
        ensure_executable(&destination.join("sg")).expect("mark executable");
        cleanup_archive(&path).expect("remove archive");
        assert!(!path.exists());
    }
}
