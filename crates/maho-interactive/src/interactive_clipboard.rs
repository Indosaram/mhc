use base64::Engine;
use tokio::io::{AsyncReadExt,AsyncWriteExt};

pub async fn read_text()->Option<String> {
    if cfg!(target_os="linux") {
        let mut commands:Vec<(&str,Vec<&str>)>=Vec::new();
        if std::env::var("TERMUX_VERSION").is_ok_and(|value|!value.is_empty()){commands.push(("termux-clipboard-get",vec![]));}
        if std::env::var("WAYLAND_DISPLAY").is_ok_and(|value|!value.is_empty()){commands.push(("wl-paste",vec!["--no-newline","--type","text"]));}
        if std::env::var("DISPLAY").is_ok_and(|value|!value.is_empty()){commands.extend([("xclip",vec!["-selection","clipboard","-out"]),("xsel",vec!["--clipboard","--output"])]);}
        for (command,args) in commands {
            let Ok(mut child)=tokio::process::Command::new(command).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn() else {continue;};
            let transfer=async {
                let mut bytes=Vec::new();
                child.stdout.take()?.take(50*1024*1024+1).read_to_end(&mut bytes).await.ok()?;
                if bytes.len()>50*1024*1024{return None;}
                if child.wait().await.ok()?.success(){Some(String::from_utf8_lossy(&bytes).into_owned())}else{None}
            };
            if let Ok(Some(text))=tokio::time::timeout(std::time::Duration::from_secs(5),transfer).await{return (!text.is_empty()).then_some(text);}
        }
    }
    maho_tui::native_platform::get_native_clipboard()?.get_text().ok().flatten().filter(|text|!text.is_empty())
}

fn is_remote_session() -> bool {
    ["SSH_CONNECTION", "SSH_CLIENT", "MOSH_CONNECTION"].iter().any(|key| std::env::var(key).is_ok_and(|value| !value.is_empty()))
}
fn encode_osc52(text: &str) -> Result<String, String> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    if encoded.len() > 100_000 { return Err("Clipboard unavailable: text exceeds the OSC 52 size limit".into()); }
    Ok(format!("\x1b]52;c;{encoded}\x07"))
}

pub async fn copy(text: &str) -> Result<Option<String>, String> {
    let mut copied = false;
    if !cfg!(target_os="linux") && let Some(clipboard) = maho_tui::native_platform::get_native_clipboard() {
        copied = clipboard.set_text(text).is_some_and(|result|result.is_ok());
    }
    if !copied {
        let mut commands: Vec<(&str,Vec<&str>)> = Vec::new();
        if cfg!(target_os="macos") { commands.push(("pbcopy",vec![])); }
        else if cfg!(target_os="windows") { commands.push(("clip",vec![])); }
        else {
            if std::env::var_os("TERMUX_VERSION").is_some() { commands.push(("termux-clipboard-set",vec![])); }
            if std::env::var_os("WAYLAND_DISPLAY").is_some() { commands.push(("wl-copy",vec![])); }
            if std::env::var_os("DISPLAY").is_some() { commands.extend([("xclip",vec!["-selection","clipboard"]),("xsel",vec!["--clipboard","--input"])]); }
        }
        for (command,args) in commands {
            let Ok(mut child) = tokio::process::Command::new(command).args(args).stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn() else { continue; };
            let transfer = async {
                if let Some(mut stdin) = child.stdin.take() { let _ = stdin.write_all(text.as_bytes()).await; }
                child.wait().await.is_ok_and(|status|status.success())
            };
            if tokio::time::timeout(std::time::Duration::from_secs(5),transfer).await.unwrap_or(false) { copied=true; break; }
        }
    }
    let headless = cfg!(target_os="linux") && std::env::var_os("DISPLAY").is_none() && std::env::var_os("WAYLAND_DISPLAY").is_none() && std::env::var_os("TERMUX_VERSION").is_none();
    let use_wsl_osc52 = cfg!(target_os="linux") && std::env::var_os("WSL_INTEROP").is_some() && std::env::var_os("WT_SESSION").is_some();
    if !copied && use_wsl_osc52 {
        return encode_osc52(text).map(Some);
    }
    if !copied && (is_remote_session() || headless) {
        return encode_osc52(text).map(Some);
    }
    if copied { return Ok(None); }
    if cfg!(target_os="linux") {
        if std::env::var_os("TERMUX_VERSION").is_some() { return Err("Clipboard unavailable: install the Termux:API app and `termux-api` package".into()); }
        if std::env::var_os("WAYLAND_DISPLAY").is_some() { return Err("Clipboard unavailable: install `wl-clipboard` (`wl-copy`) or check Wayland access".into()); }
        if std::env::var_os("DISPLAY").is_some() { return Err("Clipboard unavailable: install `xclip` or `xsel`, or check X11 access".into()); }
    }
    Err("Clipboard unavailable".into())
}

#[cfg(test)]
mod clipboard_fallback_tests {
    use super::*;
    #[test]
    fn osc52_enforces_encoded_size_limit() {
        let encoded = encode_osc52("hello").expect("small clipboard text");
        assert!(encoded.starts_with("\x1b]52;c;"));
        assert!(encoded.ends_with('\u{7}'));
        let large = "x".repeat(75_001);
        assert!(encode_osc52(&large).is_err());
    }
}

// ---- senpi `utils/clipboard-image.ts` --------------------------------------------------------

const SUPPORTED_IMAGE_MIME_TYPES: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];

/// senpi `baseMimeType`.
fn base_mime_type(mime_type: &str) -> String { mime_type.split(';').next().unwrap_or(mime_type).trim().to_ascii_lowercase() }

/// senpi `selectPreferredImageMimeType`: the first supported type, else any `image/*`.
fn select_preferred_image_mime_type(types: &[String]) -> Option<String> {
    for preferred in SUPPORTED_IMAGE_MIME_TYPES {
        if let Some(found) = types.iter().find(|value| base_mime_type(value) == preferred) { return Some(found.clone()); }
    }
    types.iter().find(|value| base_mime_type(value).starts_with("image/")).cloned()
}

/// Run a clipboard helper and capture stdout, bounded like the text path (5 s, 50 MiB).
async fn run_capture(command: &str, args: &[&str]) -> Option<Vec<u8>> {
    let mut child = tokio::process::Command::new(command).args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null()).kill_on_drop(true).spawn().ok()?;
    let transfer = async {
        let mut bytes = Vec::new();
        child.stdout.take()?.take(50 * 1024 * 1024 + 1).read_to_end(&mut bytes).await.ok()?;
        if bytes.len() > 50 * 1024 * 1024 { return None; }
        child.wait().await.ok()?.success().then_some(bytes)
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), transfer).await.ok().flatten()
}

fn listed_types(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes).lines().map(|line| line.trim().to_owned()).filter(|line| !line.is_empty()).collect()
}

/// senpi `readClipboardImageViaWlPaste`.
async fn read_image_via_wl_paste() -> Option<(Vec<u8>, String)> {
    let types = listed_types(&run_capture("wl-paste", &["--list-types"]).await?);
    let selected = select_preferred_image_mime_type(&types)?;
    let data = run_capture("wl-paste", &["--type", &selected, "--no-newline"]).await?;
    if data.is_empty() { return None; }
    Some((data, base_mime_type(&selected)))
}

/// senpi `readClipboardImageViaXclip`.
async fn read_image_via_xclip() -> Option<(Vec<u8>, String)> {
    let targets = run_capture("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"]).await;
    let candidate_types = targets.map_or_else(Vec::new, |bytes| listed_types(&bytes));
    let mut try_types: Vec<String> = select_preferred_image_mime_type(&candidate_types).into_iter().collect();
    try_types.extend(SUPPORTED_IMAGE_MIME_TYPES.iter().map(|value| (*value).to_owned()));
    for mime_type in try_types {
        if let Some(data) = run_capture("xclip", &["-selection", "clipboard", "-t", &mime_type, "-o"]).await && !data.is_empty() {
            return Some((data, base_mime_type(&mime_type)));
        }
    }
    None
}

/// senpi `readClipboardImageViaNativeClipboard`.
fn read_image_via_native() -> Option<(Vec<u8>, String)> {
    let bytes = maho_tui::native_platform::get_native_clipboard()?.get_image().ok().flatten()?;
    if bytes.is_empty() { return None; }
    let mime_type = maho_tools::read::detect_supported_image_mime_type(&bytes).unwrap_or("application/octet-stream");
    Some((bytes, mime_type.to_owned()))
}

/// senpi `readClipboardImage`: the platform clipboard bitmap as (bytes, mimeType), or `None`.
///
/// An unsupported format (BMP, TIFF, ...) is returned as-is; [`process_image`] converts it to PNG.
/// The WSL PowerShell backend is omitted.
pub async fn read_image() -> Option<(Vec<u8>, String)> {
    if std::env::var("TERMUX_VERSION").is_ok_and(|value| !value.is_empty()) { return None; }
    let mut image = None;
    if cfg!(target_os = "linux") {
        let wayland = std::env::var("WAYLAND_DISPLAY").is_ok_and(|value| !value.is_empty()) || std::env::var("XDG_SESSION_TYPE").is_ok_and(|value| value == "wayland");
        if wayland { image = read_image_via_wl_paste().await; }
        if image.is_none() { image = read_image_via_xclip().await; }
        if image.is_none() { image = read_image_via_native(); }
    } else {
        image = read_image_via_native();
    }
    image
}

/// senpi `processImage`: convert an unsupported format to PNG, then (unless disabled) resize it
/// under the inline image limits. `None` when the bytes cannot be decoded.
pub async fn process_image(bytes: &[u8], mime_type: &str, auto_resize: bool) -> Option<maho_ai::types::ImageContent> {
    let options = maho_core::utils::image_process::ProcessImageOptions { auto_resize_images: Some(auto_resize), ..Default::default() };
    let processed = maho_core::utils::image_process::process_image(bytes, mime_type, options).await.ok()?;
    Some(maho_ai::types::ImageContent { data: processed.data, mime_type: processed.mime_type })
}

/// The largest image file a pasted path may attach; matches the clipboard read bound.
const MAX_PASTED_IMAGE_FILE_BYTES: u64 = 50 * 1024 * 1024;

/// The image file a paste names, when the whole paste is one path to an existing image file.
///
/// Terminals that cannot forward a bitmap (Ferryx remote panes, drag-and-drop) save the image and
/// paste its path, optionally shell-quoted and followed by a space. Such a paste is attached as an
/// image instead of inserted as text.
pub fn pasted_image_path(pasted: &str) -> Option<std::path::PathBuf> {
    let trimmed = pasted.trim();
    if trimmed.is_empty() || trimmed.contains(['\n', '\r']) { return None; }
    let unquoted = if let Some(inner) = trimmed.strip_prefix('\'').and_then(|rest| rest.strip_suffix('\'')) {
        inner.replace("'\\''", "'")
    } else if let Some(inner) = trimmed.strip_prefix('"').and_then(|rest| rest.strip_suffix('"')) {
        inner.to_owned()
    } else {
        trimmed.replace("\\ ", " ")
    };
    let unquoted = unquoted.strip_prefix("file://").unwrap_or(&unquoted);
    let path = match unquoted.strip_prefix("~/") {
        Some(rest) => maho_core::config::home_dir_path().join(rest),
        None => std::path::PathBuf::from(unquoted),
    };
    if !path.is_absolute() { return None; }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(extension.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "tif" | "tiff") { return None; }
    let metadata = std::fs::metadata(&path).ok()?;
    (metadata.is_file() && metadata.len() > 0 && metadata.len() <= MAX_PASTED_IMAGE_FILE_BYTES).then_some(path)
}

/// Read and process the image file [`pasted_image_path`] found. `None` when it does not decode.
pub async fn read_image_file(path: &std::path::Path, auto_resize: bool) -> Option<maho_ai::types::ImageContent> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let mime_type = maho_tools::read::detect_supported_image_mime_type(&bytes).unwrap_or("application/octet-stream");
    process_image(&bytes, mime_type, auto_resize).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferred_image_mime_type_prefers_the_supported_order() {
        assert_eq!(select_preferred_image_mime_type(&["text/html".into(), "image/webp".into(), "image/png".into()]).as_deref(), Some("image/png"), "png outranks webp in the supported order");
        assert_eq!(select_preferred_image_mime_type(&["image/bmp".into()]).as_deref(), Some("image/bmp"), "an unsupported image type is still selected for the codec path");
        assert_eq!(select_preferred_image_mime_type(&["text/plain".into()]), None);
        assert_eq!(select_preferred_image_mime_type(&[]), None);
    }

    #[test]
    fn base_mime_type_normalizes_parameters() {
        assert_eq!(base_mime_type("image/png;charset=binary"), "image/png");
        assert_eq!(base_mime_type("IMAGE/PNG"), "image/png");
    }

    /// A 1x1 RGBA PNG.
    const TINY_PNG: [u8; 70] = [
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
        0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
        0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
        0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
        0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    #[tokio::test]
    async fn process_image_converts_an_unsupported_format_to_png() {
        let processed = process_image(&TINY_PNG, "image/bmp", false).await.expect("decodes");
        assert_eq!(processed.mime_type, "image/png");
        assert!(process_image(b"not an image", "image/bmp", false).await.is_none());
    }

    #[test]
    fn pasted_image_path_accepts_only_one_existing_image_file() {
        let directory = tempfile::tempdir().expect("tempdir");
        let image = directory.path().join("shot one.png");
        std::fs::write(&image, TINY_PNG).expect("write image");
        let text = directory.path().join("notes.txt");
        std::fs::write(&text, b"text").expect("write text");
        let display = image.display().to_string();
        assert_eq!(pasted_image_path(&format!("'{display}' ")), Some(image.clone()), "a quoted path with the trailing space Ferryx adds");
        assert_eq!(pasted_image_path(&format!("\"{display}\"")), Some(image.clone()));
        assert_eq!(pasted_image_path(&display.replace(' ', "\\ ")), Some(image.clone()), "a backslash-escaped path");
        assert_eq!(pasted_image_path(&format!("file://{display}")), Some(image));
        assert_eq!(pasted_image_path(&text.display().to_string()), None, "not an image extension");
        assert_eq!(pasted_image_path(&directory.path().join("missing.png").display().to_string()), None);
        assert_eq!(pasted_image_path(&format!("look at {display}")), None, "a sentence is text");
        assert_eq!(pasted_image_path("shot.png"), None, "a relative path is text");
        assert_eq!(pasted_image_path(""), None);
    }
}
