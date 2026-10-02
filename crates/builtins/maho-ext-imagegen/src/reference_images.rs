use std::{fs::OpenOptions, io::Read, path::Path};
use base64::{Engine, engine::general_purpose::STANDARD};
use maho_ai::types::ImageContent;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;

const MAX_REFERENCE_BYTES: u64 = 50 * 1024 * 1024;
fn image_mime_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a]) { Some("image/png") }
    else if bytes.starts_with(&[0xff,0xd8,0xff]) { Some("image/jpeg") }
    else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" { Some("image/webp") }
    else { None }
}
fn load_image_file(cwd: &Path, path: &str, label: &str) -> Result<ImageContent, String> {
    let load = || -> Result<ImageContent, String> {
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        options.custom_flags(libc::O_NONBLOCK);
        let file = options.open(cwd.join(path)).map_err(|e| e.to_string())?;
        let metadata = file.metadata().map_err(|e| e.to_string())?;
        if !metadata.is_file() { return Err("must be a regular file".into()); }
        if metadata.len() > MAX_REFERENCE_BYTES { return Err("must be at most 50 MB".into()); }
        let mut bytes = Vec::new();
        file.take(MAX_REFERENCE_BYTES + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
        if bytes.len() as u64 > MAX_REFERENCE_BYTES { return Err("must be at most 50 MB".into()); }
        let mime_type = image_mime_type(&bytes).ok_or("must be a PNG, JPEG, or WEBP image (invalid magic bytes)")?;
        Ok(ImageContent { data: STANDARD.encode(&bytes), mime_type: mime_type.into() })
    };
    load().map_err(|reason| format!("Error: {label} \"{path}\": {reason}"))
}
pub fn load_reference_images(cwd: &Path, paths: Option<&[String]>) -> Result<Vec<ImageContent>, String> {
    let Some(paths) = paths else { return Ok(Vec::new()); };
    if !(1..=5).contains(&paths.len()) { return Err(format!("Error: reference_image_paths must contain 1 to 5 paths (got {}).", paths.len())); }
    paths.iter().map(|path| load_image_file(cwd, path, "reference image")).collect()
}
pub fn load_mask_image(cwd: &Path, path: Option<&str>, reference_count: usize) -> Option<Result<ImageContent, String>> {
    let path = path?;
    Some(if reference_count == 0 { Err("Error: mask_image_path requires at least one reference_image_paths entry to edit.".into()) }
    else { load_image_file(cwd, path, "mask image") })
}
