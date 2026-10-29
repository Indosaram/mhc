use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::{SG_PINNED_VERSION, SgManifestAsset, runtime_slug, sg_binary_name, sg_release_asset};
use crate::contains_path::lexical_normalize;
use crate::runtime::{node_arch, node_platform};

const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const CENTRAL_SIGNATURE: u32 = 0x0201_4b50;
const LOCAL_SIGNATURE: u32 = 0x0403_4b50;
const ZIP64_SENTINEL: u32 = 0xffff_ffff;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SgProvisionErrorCode {
    BadChecksum,
    DownloadFailed,
    ExtractFailed,
    UnsupportedPlatform,
    WriteFailed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgProvisionError {
    pub code: SgProvisionErrorCode,
    pub message: String,
}

impl fmt::Display for SgProvisionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SgProvisionError {}

fn fail(code: SgProvisionErrorCode, message: String) -> SgProvisionError {
    SgProvisionError { code, message }
}

/// Minimal HTTP response the provisioner needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SgFetchResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

/// Downloads `url`; `Err` carries a transport failure description.
pub type SgFetch<'a> = &'a dyn Fn(&str) -> Result<SgFetchResponse, String>;

pub struct SgProvisionOptions<'a> {
    pub arch: Option<&'a str>,
    /// Required: no network client ships in this crate, so callers inject one.
    pub fetch: SgFetch<'a>,
    pub platform: Option<&'a str>,
    pub release_assets: Option<&'a [(&'a str, SgManifestAsset)]>,
    pub target_dir: PathBuf,
}

struct ZipEntry {
    compressed_size: u32,
    local_header_offset: u32,
    method: u16,
    name: String,
    uncompressed_size: u32,
}

fn extract_failed(message: String) -> SgProvisionError {
    fail(SgProvisionErrorCode::ExtractFailed, message)
}

fn u16_at(zip: &[u8], offset: usize) -> Result<u16, SgProvisionError> {
    zip.get(offset..offset + 2)
        .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
        .ok_or_else(|| extract_failed("downloaded ast-grep zip is truncated".to_string()))
}

fn u32_at(zip: &[u8], offset: usize) -> Result<u32, SgProvisionError> {
    zip.get(offset..offset + 4)
        .map(|bytes| u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
        .ok_or_else(|| extract_failed("downloaded ast-grep zip is truncated".to_string()))
}

fn widen(value: u32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

fn find_eocd(zip: &[u8]) -> Result<usize, SgProvisionError> {
    let not_zip = || extract_failed("downloaded ast-grep asset is not a zip archive".to_string());
    let highest = zip.len().checked_sub(22).ok_or_else(not_zip)?;
    let lowest = highest.saturating_sub(65_535);
    (lowest..=highest)
        .rev()
        .find(|offset| u32_at(zip, *offset).ok() == Some(EOCD_SIGNATURE))
        .ok_or_else(not_zip)
}

fn list_entries(zip: &[u8]) -> Result<Vec<ZipEntry>, SgProvisionError> {
    let eocd = find_eocd(zip)?;
    let count = u16_at(zip, eocd + 10)?;
    let mut cursor = widen(u32_at(zip, eocd + 16)?);
    let mut entries = Vec::new();
    for _ in 0..count {
        if cursor + 46 > zip.len() || u32_at(zip, cursor)? != CENTRAL_SIGNATURE {
            return Err(extract_failed(
                "downloaded ast-grep zip central directory is corrupt".to_string(),
            ));
        }
        let name_len = usize::from(u16_at(zip, cursor + 28)?);
        let extra_len = usize::from(u16_at(zip, cursor + 30)?);
        let comment_len = usize::from(u16_at(zip, cursor + 32)?);
        let name_bytes = zip
            .get(cursor + 46..cursor + 46 + name_len)
            .unwrap_or_default();
        entries.push(ZipEntry {
            compressed_size: u32_at(zip, cursor + 20)?,
            local_header_offset: u32_at(zip, cursor + 42)?,
            method: u16_at(zip, cursor + 10)?,
            name: String::from_utf8_lossy(name_bytes).into_owned(),
            uncompressed_size: u32_at(zip, cursor + 24)?,
        });
        cursor += 46 + name_len + extra_len + comment_len;
    }
    Ok(entries)
}

fn read_entry(zip: &[u8], entry: &ZipEntry) -> Result<Vec<u8>, SgProvisionError> {
    if [
        entry.compressed_size,
        entry.uncompressed_size,
        entry.local_header_offset,
    ]
    .contains(&ZIP64_SENTINEL)
    {
        return Err(extract_failed(format!(
            "ast-grep zip entry {} uses unsupported zip64 extensions",
            entry.name
        )));
    }
    let local = widen(entry.local_header_offset);
    if u32_at(zip, local)? != LOCAL_SIGNATURE {
        return Err(extract_failed(format!(
            "ast-grep zip entry {} has a corrupt local header",
            entry.name
        )));
    }
    let start =
        local + 30 + usize::from(u16_at(zip, local + 26)?) + usize::from(u16_at(zip, local + 28)?);
    let raw = zip
        .get(start..start + widen(entry.compressed_size))
        .ok_or_else(|| extract_failed(format!("ast-grep zip entry {} is truncated", entry.name)))?;
    let bytes = match entry.method {
        0 => raw.to_vec(),
        8 => {
            let mut inflated = Vec::new();
            flate2::read::DeflateDecoder::new(raw)
                .read_to_end(&mut inflated)
                .map_err(|error| {
                    extract_failed(format!(
                        "ast-grep zip entry {} failed to inflate: {error}",
                        entry.name
                    ))
                })?;
            inflated
        }
        method => {
            return Err(extract_failed(format!(
                "ast-grep zip entry {} uses unsupported compression method {method}",
                entry.name
            )));
        }
    };
    if bytes.len() != widen(entry.uncompressed_size) {
        return Err(extract_failed(format!(
            "ast-grep zip entry {} inflated to {} bytes, expected {}",
            entry.name,
            bytes.len(),
            entry.uncompressed_size
        )));
    }
    Ok(bytes)
}

fn extract_standalone_binary(zip: &[u8], platform: &str) -> Result<Vec<u8>, SgProvisionError> {
    let suffix = if platform == "win32" { ".exe" } else { "" };
    let entries = list_entries(zip)?;
    let preferred = [format!("ast-grep{suffix}"), format!("sg{suffix}")];
    for name in &preferred {
        if let Some(entry) = entries
            .iter()
            .find(|entry| entry.name.rsplit('/').next() == Some(name.as_str()))
        {
            return read_entry(zip, entry);
        }
    }
    Err(extract_failed(format!(
        "ast-grep release zip has no standalone {} binary",
        preferred.join(" or ")
    )))
}

fn write_binary(temp: &Path, destination: &Path, bytes: &[u8]) -> std::io::Result<()> {
    fs::write(temp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(temp, fs::Permissions::from_mode(0o755))?;
    }
    fs::rename(temp, destination)
}

/// Download the pinned release, verify its sha256, extract the standalone binary and install
/// it as `<target_dir>/sg[.exe]` via a temp file inside `target_dir`.
pub fn provision_sg_binary(options: &SgProvisionOptions<'_>) -> Result<PathBuf, SgProvisionError> {
    let platform = options.platform.unwrap_or(node_platform());
    let slug = runtime_slug(platform, options.arch.unwrap_or(node_arch()));
    let asset = options
        .release_assets
        .and_then(|assets| {
            assets
                .iter()
                .find(|(key, _)| *key == slug)
                .map(|(_, asset)| *asset)
        })
        .or_else(|| sg_release_asset(&slug))
        .ok_or_else(|| {
            fail(
                SgProvisionErrorCode::UnsupportedPlatform,
                format!("ast-grep {SG_PINNED_VERSION} has no asset for {slug}"),
            )
        })?;
    let base = if options.target_dir.is_absolute() {
        options.target_dir.clone()
    } else {
        std::env::current_dir()
            .unwrap_or_default()
            .join(&options.target_dir)
    };
    let target_dir = lexical_normalize(&base);
    let destination = target_dir.join(sg_binary_name(platform));
    let temp = target_dir.join(format!(".sg-{}.partial", std::process::id()));
    let write_failed = |error: String| {
        fail(
            SgProvisionErrorCode::WriteFailed,
            format!(
                "failed to provision ast-grep {SG_PINNED_VERSION} into {}: {error}",
                target_dir.display()
            ),
        )
    };
    fs::create_dir_all(&target_dir).map_err(|error| write_failed(error.to_string()))?;
    let download_error = |detail: String| {
        fail(
            SgProvisionErrorCode::DownloadFailed,
            format!(
                "failed to download ast-grep {SG_PINNED_VERSION} from {}: {detail}",
                asset.url
            ),
        )
    };
    let response = (options.fetch)(asset.url).map_err(download_error)?;
    if !(200..300).contains(&response.status) {
        return Err(download_error(format!("HTTP {}", response.status)));
    }
    let actual = hex::encode(Sha256::digest(&response.body));
    if actual != asset.sha256 {
        let file = asset.url.rsplit('/').next().unwrap_or(asset.url);
        return Err(fail(
            SgProvisionErrorCode::BadChecksum,
            format!(
                "checksum mismatch for {file}: expected {}, got {actual}",
                asset.sha256
            ),
        ));
    }
    let result = extract_standalone_binary(&response.body, platform).and_then(|bytes| {
        write_binary(&temp, &destination, &bytes).map_err(|error| write_failed(error.to_string()))
    });
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map(|()| destination)
}
