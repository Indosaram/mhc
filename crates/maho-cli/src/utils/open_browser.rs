pub fn open_browser(target: &str) -> std::io::Result<std::process::Child> {
    use std::process::{Command, Stdio};
    let mut command = if cfg!(target_os = "macos") { Command::new("open") } else if cfg!(target_os = "windows") { let mut c = Command::new("rundll32"); c.arg("url.dll,FileProtocolHandler"); c } else { Command::new("xdg-open") };
    command.arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
}
