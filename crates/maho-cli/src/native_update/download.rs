use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;

fn redirect_allowed(origin: &url::Url, destination: &url::Url) -> bool {
    if super::release::is_loopback_host(origin) {
        return super::release::is_loopback_host(destination)
            && matches!(destination.scheme(), "http" | "https");
    }
    let host = destination.host_str().unwrap_or_default();
    destination.scheme() == "https"
        && (host == "github.com" || host.ends_with(".githubusercontent.com"))
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .user_agent(format!("mhc/{}", super::release::current_version()))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 5 {
                return attempt.error("too many download redirects");
            }
            if attempt
                .previous()
                .first()
                .is_some_and(|origin| redirect_allowed(origin, attempt.url()))
            {
                attempt.follow()
            } else {
                attempt.error("download redirected outside GitHub HTTPS")
            }
        }))
        .build()
        .map_err(|error| error.to_string())
}

pub fn parse_checksum(text: &str, filename: &str) -> Result<String, String> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let line = lines.next().ok_or("empty checksum file")?;
    if lines.next().is_some() {
        return Err("checksum file must contain one entry".into());
    }
    let mut fields = line.split_whitespace();
    let digest = fields.next().ok_or("missing checksum")?;
    let name = fields
        .next()
        .ok_or("missing checksum filename")?
        .trim_start_matches('*');
    if digest.len() != 64
        || !digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        || name != filename
        || fields.next().is_some()
    {
        return Err("invalid checksum or mismatched archive filename".into());
    }
    Ok(digest.to_ascii_lowercase())
}

pub async fn verified_archive(
    release: &super::release::Release,
    destination: &Path,
) -> Result<(), String> {
    let client = client()?;
    let mut checksum_response = client
        .get(&release.checksum_url)
        .send()
        .await
        .map_err(|error| format!("download checksum: {error}"))?
        .error_for_status()
        .map_err(|error| format!("download checksum: {error}"))?;
    let mut checksum_bytes = Vec::new();
    while let Some(chunk) = checksum_response
        .chunk()
        .await
        .map_err(|error| error.to_string())?
    {
        if checksum_bytes.len() + chunk.len() > 4096 {
            return Err("checksum response exceeds size limit".into());
        }
        checksum_bytes.extend_from_slice(&chunk);
    }
    let expected = parse_checksum(
        std::str::from_utf8(&checksum_bytes).map_err(|error| error.to_string())?,
        &release.asset_name,
    )?;
    let mut response = client
        .get(&release.asset_url)
        .send()
        .await
        .map_err(|error| format!("download package: {error}"))?
        .error_for_status()
        .map_err(|error| format!("download package: {error}"))?;
    if response
        .content_length()
        .is_some_and(|length| length > MAX_ARCHIVE_BYTES)
    {
        return Err("package exceeds download size limit".into());
    }
    let mut output = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)
        .await
        .map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut length = 0u64;
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? {
        length = length
            .checked_add(chunk.len() as u64)
            .ok_or("download size overflow")?;
        if length > MAX_ARCHIVE_BYTES {
            return Err("package exceeds download size limit".into());
        }
        digest.update(&chunk);
        output
            .write_all(&chunk)
            .await
            .map_err(|error| error.to_string())?;
    }
    output.sync_all().await.map_err(|error| error.to_string())?;
    if format!("{:x}", digest.finalize()) != expected {
        return Err("downloaded package SHA256 does not match release checksum".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[test]
    fn redirects_keep_loopback_and_production_downloads_separate() -> Result<(), url::ParseError> {
        let production =
            url::Url::parse("https://github.com/Indosaram/mhc/releases/download/v1/mhc.zip")?;
        let local = url::Url::parse("http://127.0.0.1:1234/package")?;
        for destination in [
            "http://127.0.0.2/package",
            "https://localhost/package",
            "http://[::1]/package",
            "http://qa.localhost/package",
        ] {
            let destination = url::Url::parse(destination)?;
            assert!(redirect_allowed(&local, &destination));
            assert!(!redirect_allowed(&production, &destination));
        }
        let github_asset = url::Url::parse("https://release-assets.githubusercontent.com/package")?;
        assert!(redirect_allowed(&production, &github_asset));
        assert!(!redirect_allowed(&local, &github_asset));
        for destination in [
            "http://github.com/package",
            "https://example.com/package",
            "ftp://localhost/package",
            "https://github.com.evil.invalid/package",
        ] {
            let destination = url::Url::parse(destination)?;
            assert!(!redirect_allowed(&production, &destination));
            assert!(!redirect_allowed(&local, &destination));
        }
        Ok(())
    }

    async fn serve_package(
        corrupt: bool,
    ) -> (super::super::release::Release, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("fixture listener");
        let address = listener.local_addr().expect("fixture address");
        let release = super::super::release::Release {
            version: "2026.10.20.2".into(),
            tag: "v2026.10.20.2".into(),
            asset_name: "mhc.zip".into(),
            asset_url: format!("http://{address}/mhc.zip"),
            checksum_url: format!("http://{address}/mhc.zip.sha256"),
        };
        let server = tokio::spawn(async move {
            let package = b"actual package bytes";
            let checksum = format!("{:x}  mhc.zip\n", Sha256::digest(package));
            for body in [
                checksum.as_bytes(),
                if corrupt {
                    b"corrupt package"
                } else {
                    package.as_slice()
                },
            ] {
                let (mut socket, _) =
                    tokio::time::timeout(Duration::from_secs(5), listener.accept())
                        .await
                        .expect("bounded connection")
                        .expect("connection");
                let mut request = Vec::new();
                let mut byte = [0u8; 1];
                tokio::time::timeout(Duration::from_secs(5), async {
                    while !request.ends_with(b"\r\n\r\n") {
                        socket.read_exact(&mut byte).await.expect("request byte");
                        request.push(byte[0]);
                        assert!(request.len() < 8192, "bounded request headers");
                    }
                })
                .await
                .expect("bounded request");
                let headers = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                socket
                    .write_all(headers.as_bytes())
                    .await
                    .expect("response headers");
                socket.write_all(body).await.expect("response body");
                socket.shutdown().await.expect("close response");
            }
        });
        (release, server)
    }

    #[tokio::test]
    async fn downloaded_bytes_must_match_published_checksum() {
        for corrupt in [false, true] {
            let root = tempfile::tempdir().expect("download directory");
            let path = root.path().join("candidate.zip");
            let (release, server) = serve_package(corrupt).await;
            let result = verified_archive(&release, &path).await;
            server.await.expect("fixture completed");
            if corrupt {
                assert!(
                    result.is_err(),
                    "modified download must fail integrity verification"
                );
            } else {
                result.expect("verified download");
                assert_eq!(
                    std::fs::read(path).expect("downloaded file"),
                    b"actual package bytes"
                );
            }
        }
    }

    #[test]
    fn checksum_is_bound_to_one_named_archive() {
        let digest = "ab".repeat(32);
        assert_eq!(
            parse_checksum(&format!("{digest}  mhc.zip\n"), "mhc.zip"),
            Ok(digest.clone())
        );
        assert!(parse_checksum(&format!("{digest}  other.zip"), "mhc.zip").is_err());
        assert!(
            parse_checksum(
                &format!("{digest}  mhc.zip\n{digest}  other.zip"),
                "mhc.zip"
            )
            .is_err()
        );
        assert!(parse_checksum("invalid  mhc.zip", "mhc.zip").is_err());
    }
}
