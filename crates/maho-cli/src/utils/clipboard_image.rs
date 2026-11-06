use super::clipboard_command::{ClipboardCommandOptions, run_clipboard_command};
use std::collections::BTreeMap;
pub struct ClipboardImage {
    pub bytes: Vec<u8>,
    pub mime_type: String,
}
enum BackendResult {
    Unavailable,
    Empty,
    Image(ClipboardImage),
}
const SUPPORTED: [&str; 4] = ["image/png", "image/jpeg", "image/webp", "image/gif"];
fn base_mime_type(mime: &str) -> String {
    mime.split(';').next().unwrap_or(mime).trim().to_lowercase()
}
pub fn is_wayland_session(env: &BTreeMap<String, String>) -> bool {
    env.get("WAYLAND_DISPLAY")
        .is_some_and(|value| !value.is_empty())
        || env
            .get("XDG_SESSION_TYPE")
            .is_some_and(|value| value == "wayland")
}
pub fn extension_for_image_mime_type(mime: &str) -> Option<&'static str> {
    match base_mime_type(mime).as_str() {
        "image/png" => Some("png"),
        "image/jpeg" => Some("jpg"),
        "image/webp" => Some("webp"),
        "image/gif" => Some("gif"),
        _ => None,
    }
}
pub fn select_preferred_image_mime_type(types: &[String]) -> Option<String> {
    let normalized: Vec<_> = types
        .iter()
        .map(|value| value.trim())
        .filter(|value| !value.is_empty())
        .collect();
    for preferred in SUPPORTED {
        if let Some(value) = normalized
            .iter()
            .find(|value| base_mime_type(value) == preferred)
        {
            return Some((*value).to_owned());
        }
    }
    normalized
        .into_iter()
        .find(|value| base_mime_type(value).starts_with("image/"))
        .map(str::to_owned)
}
/// The commands and native clipboard `readClipboardImage` reads through (senpi `runClipboardCommand`
/// and `getNativeClipboard`), injectable so the backend order can be tested without a desktop.
pub trait ClipboardIo: Send + Sync {
    fn run_command<'a>(
        &'a self,
        command: &'a str,
        args: &'a [&'a str],
        timeout_ms: Option<u64>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<u8>>> + Send + 'a>>;
    /// `None` when there is no native helper; otherwise its `getImage` answer.
    fn native_image(&self) -> Option<Result<Option<Vec<u8>>, String>>;
    /// senpi `isWSL`: the WSL env markers, else `/proc/version`.
    fn is_wsl(&self, env: &BTreeMap<String, String>) -> bool {
        ["WSL_DISTRO_NAME", "WSL_INTEROP", "WSLENV"]
            .into_iter()
            .any(|key| env.get(key).is_some_and(|value| !value.is_empty()))
            || std::fs::read_to_string("/proc/version").is_ok_and(|release| {
                let release = release.to_lowercase();
                release.contains("microsoft") || release.contains("wsl")
            })
    }
}
struct ProcessClipboardIo;
impl ClipboardIo for ProcessClipboardIo {
    fn run_command<'a>(
        &'a self,
        command: &'a str,
        args: &'a [&'a str],
        timeout_ms: Option<u64>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Option<Vec<u8>>> + Send + 'a>> {
        Box::pin(run_clipboard_command(
            command,
            args,
            ClipboardCommandOptions {
                timeout_ms,
                ..Default::default()
            },
        ))
    }
    fn native_image(&self) -> Option<Result<Option<Vec<u8>>, String>> {
        maho_tui::native_platform::get_native_clipboard().map(|clipboard| clipboard.get_image())
    }
}
fn lines(bytes: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}
// Unavailable means the backend failed; Empty means it has no image. An empty Wayland clipboard must
// not fall through to stale X11 clipboard contents.
async fn wayland(io: &dyn ClipboardIo) -> BackendResult {
    let Some(list) = io
        .run_command("wl-paste", &["--list-types"], Some(1000))
        .await
    else {
        return BackendResult::Unavailable;
    };
    let Some(mime) = select_preferred_image_mime_type(&lines(&list)) else {
        return BackendResult::Empty;
    };
    match io
        .run_command("wl-paste", &["--type", &mime, "--no-newline"], None)
        .await
    {
        None => BackendResult::Unavailable,
        Some(bytes) if bytes.is_empty() => BackendResult::Empty,
        Some(bytes) => BackendResult::Image(ClipboardImage {
            bytes,
            mime_type: base_mime_type(&mime),
        }),
    }
}
// senpi `readClipboardImageViaXclip`: only the advertised type is read; a failed TARGETS listing
// probes nothing (#9786).
async fn xclip(io: &dyn ClipboardIo) -> BackendResult {
    let Some(targets) = io
        .run_command(
            "xclip",
            &["-selection", "clipboard", "-t", "TARGETS", "-o"],
            Some(1000),
        )
        .await
    else {
        return BackendResult::Unavailable;
    };
    let Some(mime) = select_preferred_image_mime_type(&lines(&targets)) else {
        return BackendResult::Empty;
    };
    match io
        .run_command(
            "xclip",
            &["-selection", "clipboard", "-t", &mime, "-o"],
            None,
        )
        .await
    {
        None => BackendResult::Unavailable,
        Some(bytes) if bytes.is_empty() => BackendResult::Empty,
        Some(bytes) => BackendResult::Image(ClipboardImage {
            bytes,
            mime_type: base_mime_type(&mime),
        }),
    }
}
fn native(io: &dyn ClipboardIo) -> Result<BackendResult, String> {
    let Some(result) = io.native_image() else {
        return Ok(BackendResult::Unavailable);
    };
    native_result(result)
}
fn native_result(result: Result<Option<Vec<u8>>, String>) -> Result<BackendResult, String> {
    Ok(match result? {
        None => BackendResult::Empty,
        Some(bytes) if bytes.is_empty() => BackendResult::Empty,
        Some(bytes) => {
            let mime_type = super::mime::detect_supported_image_mime_type(&bytes)
                .unwrap_or("application/octet-stream")
                .to_owned();
            BackendResult::Image(ClipboardImage { bytes, mime_type })
        }
    })
}
async fn powershell(io: &dyn ClipboardIo) -> Option<ClipboardImage> {
    let temporary = tempfile::Builder::new()
        .prefix("pi-wsl-clip-")
        .suffix(".png")
        .tempfile()
        .ok()?;
    let path = temporary.path().to_str()?;
    let windows_path = io.run_command("wslpath", &["-w", path], Some(1000)).await?;
    let windows_path = String::from_utf8_lossy(&windows_path);
    let windows_path = windows_path.trim();
    if windows_path.is_empty() {
        return None;
    }
    let quoted = windows_path.replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; Add-Type -AssemblyName System.Drawing; $path = '{quoted}'; $img = [System.Windows.Forms.Clipboard]::GetImage(); if ($img) {{ $img.Save($path, [System.Drawing.Imaging.ImageFormat]::Png); Write-Output 'ok' }} else {{ Write-Output 'empty' }}"
    );
    let output = io
        .run_command(
            "powershell.exe",
            &["-NoProfile", "-Command", &script],
            Some(5000),
        )
        .await?;
    if String::from_utf8_lossy(&output).trim() != "ok" {
        return None;
    }
    let bytes = tokio::fs::read(temporary.path()).await.ok()?;
    (!bytes.is_empty()).then_some(ClipboardImage {
        bytes,
        mime_type: "image/png".to_owned(),
    })
}
pub async fn read_clipboard_image(
    env: &BTreeMap<String, String>,
    platform: &str,
) -> Result<Option<ClipboardImage>, String> {
    read_clipboard_image_with(env, platform, &ProcessClipboardIo).await
}
/// senpi `readClipboardImage` over an explicit command/native-clipboard seam.
pub async fn read_clipboard_image_with(
    env: &BTreeMap<String, String>,
    platform: &str,
    io: &dyn ClipboardIo,
) -> Result<Option<ClipboardImage>, String> {
    if env
        .get("TERMUX_VERSION")
        .is_some_and(|value| !value.is_empty())
    {
        return Ok(None);
    }
    let result = if platform == "linux" {
        let wsl = io.is_wsl(env);
        let mut result = if is_wayland_session(env) || wsl {
            wayland(io).await
        } else {
            BackendResult::Unavailable
        };
        if matches!(result, BackendResult::Unavailable) {
            result = xclip(io).await;
        }
        // Preserve Linux's empty/unavailable distinction if Windows has no image.
        if wsl
            && !matches!(result, BackendResult::Image(_))
            && let Some(image) = powershell(io).await
        {
            result = BackendResult::Image(image);
        }
        if matches!(result, BackendResult::Unavailable) {
            result = native(io)?;
        }
        result
    } else {
        native(io)?
    };
    Ok(match result {
        BackendResult::Unavailable | BackendResult::Empty => None,
        BackendResult::Image(image) => {
            if SUPPORTED.contains(&base_mime_type(&image.mime_type).as_str()) {
                Some(image)
            } else {
                std::panic::catch_unwind(|| {
                    photon_rs::PhotonImage::new_from_byteslice(image.bytes).get_bytes()
                })
                .ok()
                .map(|bytes| ClipboardImage {
                    bytes,
                    mime_type: "image/png".to_owned(),
                })
            }
        }
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_errors_remain_errors() {
        assert!(native_result(Err("transfer failed".to_owned())).is_err());
    }
}
