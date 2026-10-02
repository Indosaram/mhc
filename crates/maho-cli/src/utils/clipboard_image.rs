use super::clipboard_command::{ClipboardCommandOptions, run_clipboard_command};
use std::collections::BTreeMap;
pub struct ClipboardImage { pub bytes: Vec<u8>, pub mime_type: String }
enum BackendResult { Unavailable, Empty, Image(ClipboardImage) }
const SUPPORTED: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];
fn base_mime_type(mime: &str) -> String { mime.split(';').next().unwrap_or(mime).trim().to_lowercase() }
pub fn is_wayland_session(env: &BTreeMap<String, String>) -> bool { env.get("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()) || env.get("XDG_SESSION_TYPE").is_some_and(|value| value == "wayland") }
pub fn extension_for_image_mime_type(mime: &str) -> Option<&'static str> { match base_mime_type(mime).as_str() { "image/png" => Some("png"), "image/jpeg" => Some("jpg"), "image/webp" => Some("webp"), "image/gif" => Some("gif"), _ => None } }
pub fn select_preferred_image_mime_type(types: &[String]) -> Option<String> {
    let normalized: Vec<_> = types.iter().map(|value| value.trim()).filter(|value| !value.is_empty()).collect();
    for preferred in SUPPORTED { if let Some(value) = normalized.iter().find(|value| base_mime_type(value) == preferred) { return Some((*value).to_owned()); } }
    normalized.into_iter().find(|value| base_mime_type(value).starts_with("image/")).map(str::to_owned)
}
async fn wayland() -> BackendResult {
    let Some(list) = run_clipboard_command("wl-paste", &["--list-types"], ClipboardCommandOptions { timeout_ms: Some(1000), ..Default::default() }).await else { return BackendResult::Unavailable; };
    let types = String::from_utf8_lossy(&list).lines().map(str::to_owned).collect::<Vec<_>>();
    let Some(mime) = select_preferred_image_mime_type(&types) else { return BackendResult::Empty; };
    match run_clipboard_command("wl-paste", &["--type", &mime, "--no-newline"], Default::default()).await {
        None => BackendResult::Unavailable, Some(bytes) if bytes.is_empty() => BackendResult::Empty,
        Some(bytes) => BackendResult::Image(ClipboardImage { bytes, mime_type: base_mime_type(&mime) }),
    }
}
async fn xclip() -> BackendResult {
    let targets = run_clipboard_command("xclip", &["-selection", "clipboard", "-t", "TARGETS", "-o"], ClipboardCommandOptions { timeout_ms: Some(1000), ..Default::default() }).await;
    let types = targets.as_ref().map(|bytes| String::from_utf8_lossy(bytes).lines().map(str::to_owned).collect::<Vec<_>>()).unwrap_or_default();
    let preferred = select_preferred_image_mime_type(&types);
    if targets.is_some() && preferred.is_none() { return BackendResult::Empty; }
    let mut candidates = preferred.into_iter().collect::<Vec<_>>();
    for mime in SUPPORTED { if !candidates.iter().any(|candidate| candidate == mime) { candidates.push(mime.to_owned()); } }
    for mime in candidates {
        if let Some(bytes) = run_clipboard_command("xclip", &["-selection", "clipboard", "-t", &mime, "-o"], Default::default()).await && !bytes.is_empty() { return BackendResult::Image(ClipboardImage { bytes, mime_type: base_mime_type(&mime) }); }
    }
    BackendResult::Unavailable
}
fn native() -> BackendResult {
    match maho_tui::native_platform::get_native_clipboard().and_then(|clipboard| clipboard.get_image().ok()) {
        None => BackendResult::Unavailable, Some(None) => BackendResult::Empty,
        Some(Some(bytes)) if bytes.is_empty() => BackendResult::Empty,
        Some(Some(bytes)) => { let mime_type = super::mime::detect_supported_image_mime_type(&bytes).unwrap_or("application/octet-stream").to_owned(); BackendResult::Image(ClipboardImage { bytes, mime_type }) }
    }
}
pub async fn read_clipboard_image(env: &BTreeMap<String, String>, platform: &str) -> Option<ClipboardImage> {
    if env.get("TERMUX_VERSION").is_some_and(|value| !value.is_empty()) { return None; }
    let mut result = if platform == "linux" { if is_wayland_session(env) { wayland().await } else { BackendResult::Unavailable } } else { native() };
    if platform == "linux" && matches!(result, BackendResult::Unavailable) { result = xclip().await; }
    if matches!(result, BackendResult::Unavailable) { result = native(); }
    match result {
        BackendResult::Unavailable | BackendResult::Empty => None,
        BackendResult::Image(image) => {
            if SUPPORTED.contains(&base_mime_type(&image.mime_type).as_str()) { Some(image) }
            else {
                let bytes = std::panic::catch_unwind(|| photon_rs::PhotonImage::new_from_byteslice(image.bytes).get_bytes()).ok()?;
                Some(ClipboardImage { bytes, mime_type: "image/png".to_owned() })
            }
        }
    }
}
