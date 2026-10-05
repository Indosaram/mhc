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
    let remote = ["SSH_CONNECTION","SSH_CLIENT","MOSH_CONNECTION"].iter().any(|key|std::env::var(key).is_ok_and(|value|!value.is_empty()));
    if remote || !copied {
        let encoded = base64::engine::general_purpose::STANDARD.encode(text);
        if encoded.len() <= 100_000 { return Ok(Some(format!("\x1b]52;c;{encoded}\x07"))); }
    }
    if copied { Ok(None) } else { Err("Failed to copy to clipboard".into()) }
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

/// senpi `isSupportedImageMimeType`.
fn is_supported_image_mime_type(mime_type: &str) -> bool { SUPPORTED_IMAGE_MIME_TYPES.contains(&base_mime_type(mime_type).as_str()) }

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
    Some((bytes, "application/octet-stream".to_owned()))
}

/// senpi `readClipboardImage`: the platform clipboard bitmap as (bytes, mimeType), or `None`.
///
/// The WSL PowerShell backend and the Photon conversion of unsupported formats are omitted: the
/// Rust port has no image codec (see `components/tool_execution_images.rs`), so an unsupported
/// payload is reported as no image rather than attached mislabelled.
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
    image.filter(|(_, mime_type)| is_supported_image_mime_type(mime_type))
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
    fn base_and_supported_mime_types_normalize_parameters() {
        assert_eq!(base_mime_type("image/png;charset=binary"), "image/png");
        assert!(is_supported_image_mime_type("image/jpeg"));
        assert!(is_supported_image_mime_type("IMAGE/PNG"));
        assert!(!is_supported_image_mime_type("image/bmp"), "an unsupported format is not attached without a codec");
        assert!(!is_supported_image_mime_type("application/octet-stream"));
    }
}
