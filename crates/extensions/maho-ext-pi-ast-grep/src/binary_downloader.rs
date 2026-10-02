use std::path::Path;

pub async fn download_archive(url: &str, archive: &Path) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let mut response = reqwest::Client::new().get(url).send().await?;
    if !response.status().is_success() {
        return Err(crate::errors::AstGrepDownloadError { message: format!("HTTP {}: {}", response.status().as_u16(), response.status().canonical_reason().unwrap_or_default()) }.into());
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
    #[tokio::test]
    async fn http_failure_preserves_download_error_type() {
        use std::io::{BufRead, Write};
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind download fixture");
        let address = listener.local_addr().expect("fixture address");
        let peer = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("download request");
            stream.set_read_timeout(Some(std::time::Duration::from_secs(2))).expect("bound request");
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone HTTP stream"));
            loop {
                let mut line = String::new();
                assert!(reader.read_line(&mut line).expect("read HTTP header") > 0);
                if line == "\r\n" { break; }
            }
            stream.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").expect("respond HTTP failure");
        });
        let fixture = tempfile::tempdir().expect("download output fixture");
        let result = download_archive(&format!("http://{address}/missing.zip"), &fixture.path().join("missing.zip")).await;
        peer.join().expect("join HTTP peer");
        let error = result.expect_err("expected HTTP error");
        let error = error.downcast_ref::<crate::errors::AstGrepDownloadError>().expect("typed HTTP error");
        assert_eq!(error.message, "HTTP 404: Not Found");
    }
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
