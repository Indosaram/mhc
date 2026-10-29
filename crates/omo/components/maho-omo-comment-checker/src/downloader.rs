//! Release downloader and extractor (port of `src/downloader.ts`).

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use comment_checker_core::{
    COMMENT_CHECKER_RELEASE_VERSION, CommentCheckerArchiveExtension, comment_checker_binary_name,
    is_cached_comment_checker_current, record_cached_comment_checker_release,
    resolve_comment_checker_release_asset,
};
use utils::archive_entry_validator::{
    ArchiveEntry, ArchiveEntryType, validate_archive_entries,
};

use crate::resolver::default_comment_checker_cache_dir;
use crate::types::SenpiCommentCheckerDownloadOptions;

pub async fn download_senpi_comment_checker_binary(
    options: &SenpiCommentCheckerDownloadOptions,
) -> Option<PathBuf> {
    let platform = options.platform.as_deref().unwrap_or(if cfg!(windows) {
        "win32"
    } else if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    });
    let arch = options.arch.as_deref().unwrap_or(if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x64"
    });

    let asset = resolve_comment_checker_release_asset(platform, arch, None);
    let Some(asset) = asset else {
        if let Some(logger) = &options.logger {
            let details = serde_json::json!({ "platform": platform, "arch": arch });
            logger.warn(
                "omo-senpi comment-checker has no pinned release for this platform",
                Some(&details),
            );
        }
        return None;
    };

    let cache_dir = options
        .cache_dir
        .clone()
        .unwrap_or_else(|| default_comment_checker_cache_dir(platform, None));
    let binary_path = cache_dir.join(comment_checker_binary_name(platform));

    if binary_path.exists() && is_cached_comment_checker_current(&cache_dir, None) {
        return Some(binary_path);
    }

    let url = options
        .resolve_asset_url
        .as_ref()
        .map_or_else(|| asset.url.clone(), |resolve| resolve(&asset));
    let archive_path = cache_dir.join(&asset.asset_name);

    if let Some(logger) = &options.logger {
        let details = serde_json::json!({
            "url": &url,
            "cacheDir": cache_dir.display().to_string(),
        });
        logger.info(
            &format!(
                "omo-senpi comment-checker: downloading release v{COMMENT_CHECKER_RELEASE_VERSION}"
            ),
            Some(&details),
        );
    }

    let download_and_extract = async {
        fs::create_dir_all(&cache_dir)?;
        download_archive(&url, &archive_path).await?;
        match asset.ext {
            CommentCheckerArchiveExtension::TarGz => {
                extract_tar_gz(&archive_path, &cache_dir)?;
            }
            CommentCheckerArchiveExtension::Zip => {
                extract_zip_archive(&archive_path, &cache_dir)?;
            }
        }
        ensure_executable(&binary_path)?;
        if !binary_path.exists() {
            return Err(io::Error::other(
                "omo-senpi comment-checker archive did not contain the expected binary",
            ));
        }
        record_cached_comment_checker_release(&cache_dir)?;
        Ok::<(), io::Error>(())
    };

    let result = download_and_extract.await;
    cleanup_archive(&archive_path);

    match result {
        Ok(()) => Some(binary_path),
        Err(err) => {
            if let Some(logger) = &options.logger {
                let details = serde_json::json!({
                    "url": &url,
                    "error": err.to_string(),
                });
                logger.warn(
                    "omo-senpi comment-checker download failed",
                    Some(&details),
                );
            }
            None
        }
    }
}

async fn download_archive(url: &str, destination: &Path) -> io::Result<()> {
    let client = reqwest::Client::new();
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| io::Error::other(format!("download request failed: {e}")))?;

    if !response.status().is_success() {
        return Err(io::Error::other(format!(
            "HTTP {}: {}",
            response.status().as_u16(),
            response.status().canonical_reason().unwrap_or_default()
        )));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| io::Error::other(format!("failed to read response bytes: {e}")))?;

    fs::write(destination, &bytes)?;
    Ok(())
}

fn extract_tar_gz(archive_path: &Path, destination_dir: &Path) -> io::Result<()> {
    let status = Command::new("tar")
        .arg("-xzf")
        .arg(archive_path)
        .arg("-C")
        .arg(destination_dir)
        .status()
        .map_err(|e| io::Error::other(format!("failed to execute tar: {e}")))?;

    if !status.success() {
        return Err(io::Error::other(format!(
            "tar extraction failed with exit code {:?}",
            status.code()
        )));
    }
    Ok(())
}

fn extract_zip_archive(archive: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir_all(destination)?;
    let mut zip_file = zip::ZipArchive::new(fs::File::open(archive)?)
        .map_err(|e| io::Error::other(format!("failed to open zip: {e}")))?;

    let mut entries = Vec::new();
    for index in 0..zip_file.len() {
        let entry = zip_file
            .by_index(index)
            .map_err(|e| io::Error::other(format!("failed to read zip entry: {e}")))?;
        let name = entry.name().replace('\\', "/");
        let is_dir = entry.is_dir();
        entries.push(ArchiveEntry {
            path: name,
            entry_type: if is_dir {
                ArchiveEntryType::Directory
            } else {
                ArchiveEntryType::File
            },
            link_path: None,
        });
    }

    validate_archive_entries(&entries, destination)
        .map_err(|e| io::Error::other(format!("unsafe archive: {e}")))?;

    for index in 0..zip_file.len() {
        let mut entry = zip_file
            .by_index(index)
            .map_err(|e| io::Error::other(format!("failed to read zip entry: {e}")))?;
        let name = entry.name().replace('\\', "/");
        if name.starts_with("__MACOSX/") {
            continue;
        }
        let outpath = destination.join(&name);
        if entry.is_dir() {
            fs::create_dir_all(&outpath)?;
        } else {
            if let Some(p) = outpath.parent() {
                fs::create_dir_all(p)?;
            }
            let mut outfile = fs::File::create(&outpath)?;
            io::copy(&mut entry, &mut outfile)?;
        }
    }
    Ok(())
}

fn ensure_executable(path: &Path) -> io::Result<()> {
    #[cfg(not(unix))]
    let _ = path;
    #[cfg(unix)]
    if path.exists() {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn cleanup_archive(path: &Path) {
    if path.exists() {
        let _ = fs::remove_file(path);
    }
}
