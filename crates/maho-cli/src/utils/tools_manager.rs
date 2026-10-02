use std::{collections::BTreeMap, path::Path};
#[derive(Clone, Copy)]
pub enum Tool { Fd, Rg }
impl Tool {
    pub const fn binary(self) -> &'static str { match self { Self::Fd => "fd", Self::Rg => "rg" } }
    pub const fn repo(self) -> &'static str { match self { Self::Fd => "sharkdp/fd", Self::Rg => "BurntSushi/ripgrep" } }
}
pub fn asset_name(tool: Tool, version: &str, platform: &str, architecture: &str) -> Option<String> {
    let arch = if architecture == "arm64" { "aarch64" } else { "x86_64" };
    let (target, extension) = match platform { "darwin" => ("apple-darwin", "tar.gz"), "linux" => ("unknown-linux-musl", "tar.gz"), "win32" => ("pc-windows-msvc", "zip"), _ => return None };
    let prefix = match tool { Tool::Fd => "fd-v", Tool::Rg => "ripgrep-" };
    Some(format!("{prefix}{version}-{arch}-{target}.{extension}"))
}
pub fn get_tool_path_in(tool: Tool, bin: &Path, env: &BTreeMap<String, String>, platform: &str) -> Option<String> {
    let local = bin.join(format!("{}{}", tool.binary(), if platform == "win32" { ".exe" } else { "" }));
    if local.exists() { return Some(local.to_string_lossy().into_owned()); }
    let names: &[&str] = match tool { Tool::Fd => &["fd", "fdfind"], Tool::Rg => &["rg"] };
    let path = env.get("PATH")?;
    let separator = if platform == "win32" { ';' } else { ':' };
    for name in names {
        let mut candidates = vec![(*name).to_owned()];
        if platform == "win32" { for extension in env.get("PATHEXT").map_or(".COM;.EXE;.BAT;.CMD", String::as_str).split(';').filter(|extension| !extension.is_empty()) { candidates.push(format!("{name}{}", extension.to_lowercase())); } }
        for directory in path.split(separator).filter(|directory| !directory.is_empty()) { for candidate in &candidates {
            if let Ok(metadata) = std::fs::metadata(Path::new(directory).join(candidate)) && metadata.is_file() {
                #[cfg(unix)]
                let executable = { use std::os::unix::fs::PermissionsExt; metadata.permissions().mode() & 0o111 != 0 };
                #[cfg(not(unix))]
                let executable = false;
                if platform == "win32" || executable { return Some((*name).to_owned()); }
            }
        } }
    }
    None
}
pub fn get_tool_path(tool: Tool) -> Option<String> { get_tool_path_in(tool, Path::new(&crate::config::get_bin_dir()), &std::env::vars().collect(), if cfg!(windows) { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS }) }
pub fn release_version_from_redirect(repo: &str, status: u16, location: Option<&str>) -> Result<String, String> {
    let location = location.filter(|_| (300..400).contains(&status)).ok_or_else(|| format!("Failed to resolve latest {repo} release: HTTP {status} without redirect"))?;
    let url = url::Url::parse("https://github.com").map_err(|error| error.to_string())?.join(location).map_err(|error| error.to_string())?;
    let tag = url.path().rsplit('/').next().unwrap_or_default();
    if tag.is_empty() || !location.contains("/releases/tag/") { return Err(format!("Failed to resolve latest {repo} release: unexpected redirect to {location}")); }
    let bytes = tag.as_bytes();
    if bytes.iter().enumerate().any(|(index, byte)| *byte == b'%' && !bytes.get(index + 1..index + 3).is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit))) { return Err("Invalid release tag encoding".to_owned()); }
    let tag = percent_encoding::percent_decode_str(tag).decode_utf8().map_err(|error| error.to_string())?;
    Ok(tag.strip_prefix('v').unwrap_or(&tag).to_owned())
}
pub async fn get_latest_version(repo: &str) -> Result<String, String> {
    let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().map_err(|error| error.to_string())?;
    let request = client.get(format!("https://github.com/{repo}/releases/latest")).header("User-Agent", "mhc-coding-agent").build().map_err(|error| error.to_string())?;
    let response = super::management_http::fetch_with_retry(&client, request, None, super::management_http::FetchRetryOptions { timeout_ms: Some(10000), ..Default::default() }).await?;
    release_version_from_redirect(repo, response.status().as_u16(), response.headers().get("location").and_then(|location| location.to_str().ok()))
}
async fn extraction_command(command: &str, args: &[&str]) -> Result<(), String> {
    let output = tokio::process::Command::new(command).args(args).output().await.map_err(|error| format!("{command}: {error}"))?;
    if output.status.success() { return Ok(()); }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let message = if !stderr.trim().is_empty() { stderr.trim().to_owned() } else if !stdout.trim().is_empty() { stdout.trim().to_owned() } else { format!("exit status {}", output.status.code().map_or("unknown".to_owned(), |code| code.to_string())) };
    Err(format!("{command}: {message}"))
}
pub async fn install_archive(archive: &Path, bin: &Path, binary_name: &str, asset: &str, platform: &str) -> Result<String, String> {
    let extract = tempfile::Builder::new().prefix(&format!("extract_tmp_{binary_name}_")).tempdir_in(bin).map_err(|error| error.to_string())?;
    let archive_text = archive.to_string_lossy();
    let destination = extract.path().to_string_lossy();
    let extraction = async {
        if asset.ends_with(".tar.gz") { return extraction_command("tar", &["xzf", &archive_text, "-C", &destination]).await; }
        if !asset.ends_with(".zip") { return Err(format!("Unsupported archive format: {asset}")); }
        let mut failures = Vec::new();
        if platform == "win32" {
            let system_tar = std::env::var("SystemRoot").or_else(|_| std::env::var("WINDIR")).ok().map(|root| Path::new(&root).join("System32/tar.exe")).filter(|path| path.exists());
            let command = system_tar.as_ref().map_or("tar.exe".into(), |path| path.to_string_lossy());
            match extraction_command(&command, &["xf", &archive_text, "-C", &destination]).await { Ok(()) => return Ok(()), Err(error) => failures.push(error) }
            let script = "& { param($archive, $destination) $ErrorActionPreference = 'Stop'; Expand-Archive -LiteralPath $archive -DestinationPath $destination -Force }";
            match extraction_command("powershell.exe", &["-NoLogo", "-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script, &archive_text, &destination]).await { Ok(()) => return Ok(()), Err(error) => failures.push(error) }
        } else {
            match extraction_command("unzip", &["-q", &archive_text, "-d", &destination]).await { Ok(()) => return Ok(()), Err(error) => failures.push(error) }
            match extraction_command("tar", &["xf", &archive_text, "-C", &destination]).await { Ok(()) => return Ok(()), Err(error) => failures.push(error) }
        }
        Err(failures.join("; "))
    }.await;
    let result = async {
        extraction.map_err(|error| format!("Failed to extract {asset}: {error}"))?;
        let stem = asset.strip_suffix(".tar.gz").or_else(|| asset.strip_suffix(".zip")).unwrap_or(asset);
        let candidates = [extract.path().join(stem).join(binary_name), extract.path().join(binary_name)];
        let mut found = candidates.into_iter().find(|path| path.exists());
        if found.is_none() {
            let mut stack = vec![extract.path().to_path_buf()];
            while let Some(directory) = stack.pop() {
                for entry in std::fs::read_dir(directory).map_err(|error| error.to_string())? {
                    let entry = entry.map_err(|error| error.to_string())?;
                    let kind = entry.file_type().map_err(|error| error.to_string())?;
                    if kind.is_file() && entry.file_name() == binary_name { found = Some(entry.path()); break; }
                    if kind.is_dir() { stack.push(entry.path()); }
                }
                if found.is_some() { break; }
            }
        }
        let found = found.ok_or_else(|| format!("Binary not found in archive: expected {binary_name} under {}", extract.path().display()))?;
        let binary = bin.join(binary_name);
        std::fs::rename(found, &binary).map_err(|error| error.to_string())?;
        #[cfg(unix)]
        if platform != "win32" { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).map_err(|error| error.to_string())?; }
        Ok(binary.to_string_lossy().into_owned())
    }.await;
    let cleanup = std::fs::remove_file(archive);
    if let Err(error) = cleanup && error.kind() != std::io::ErrorKind::NotFound { return Err(error.to_string()); }
    result
}
async fn download_tool(tool: Tool, bin: &Path, platform: &str, architecture: &str) -> Result<String, String> {
    let version = if matches!(tool, Tool::Fd) && platform == "darwin" && architecture == "x64" { "10.3.0".to_owned() } else { get_latest_version(tool.repo()).await? };
    let asset = asset_name(tool, &version, platform, architecture).ok_or_else(|| format!("Unsupported platform: {platform}/{architecture}"))?;
    std::fs::create_dir_all(bin).map_err(|error| error.to_string())?;
    let prefix = if matches!(tool, Tool::Fd) { "v" } else { "" };
    let url = format!("https://github.com/{}/releases/download/{prefix}{version}/{asset}", tool.repo());
    let client = reqwest::Client::new();
    let request = client.get(&url).build().map_err(|error| error.to_string())?;
    let mut response = super::management_http::fetch_with_retry(&client, request, None, super::management_http::FetchRetryOptions { timeout_ms: Some(120000), ..Default::default() }).await?;
    if !response.status().is_success() { return Err(format!("Download failed with HTTP {}: {url}", response.status().as_u16())); }
    let archive = bin.join(&asset);
    let mut file = tokio::fs::File::create(&archive).await.map_err(|error| error.to_string())?;
    use tokio::io::AsyncWriteExt;
    while let Some(chunk) = response.chunk().await.map_err(|error| error.to_string())? { file.write_all(&chunk).await.map_err(|error| error.to_string())?; }
    file.shutdown().await.map_err(|error| error.to_string())?;
    drop(file);
    let binary = format!("{}{}", tool.binary(), if platform == "win32" { ".exe" } else { "" });
    install_archive(&archive, bin, &binary, &asset, platform).await
}
pub struct ToolStatus { pub kind: &'static str, pub message: String }
pub async fn ensure_tool(tool: Tool, on_status: impl FnMut(ToolStatus)) -> Option<String> {
    let platform = if cfg!(windows) { "win32" } else if cfg!(target_os = "macos") { "darwin" } else { std::env::consts::OS };
    let architecture = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };
    ensure_tool_in(tool, Path::new(&crate::config::get_bin_dir()), &std::env::vars().collect(), platform, architecture, on_status).await
}
pub async fn ensure_tool_in(tool: Tool, bin: &Path, env: &BTreeMap<String, String>, platform: &str, architecture: &str, mut on_status: impl FnMut(ToolStatus)) -> Option<String> {
    if let Some(path) = get_tool_path_in(tool, bin, env, platform) { return Some(path); }
    let name = if matches!(tool, Tool::Fd) { "fd" } else { "ripgrep" };
    if env.get("MAHO_OFFLINE").or_else(|| env.get("PI_OFFLINE")).is_some_and(|value| matches!(value.to_lowercase().as_str(), "1" | "true" | "yes")) {
        on_status(ToolStatus { kind: "warning", message: format!("{name} not found. Offline mode enabled, skipping download.") }); return None;
    }
    if platform == "android" { on_status(ToolStatus { kind: "warning", message: format!("{name} not found. Install with: pkg install {name}") }); return None; }
    on_status(ToolStatus { kind: "info", message: format!("{name} not found. Downloading...") });
    match download_tool(tool, bin, platform, architecture).await {
        Ok(path) => { on_status(ToolStatus { kind: "info", message: format!("{name} installed to {path}") }); Some(path) }
        Err(error) => { on_status(ToolStatus { kind: "warning", message: format!("Failed to download {name}: {error}") }); None }
    }
}
