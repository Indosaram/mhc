pub fn open_browser(target: &str) {
    use std::process::{Command, Stdio};
    let mut command = if cfg!(target_os = "macos") { Command::new("open") } else if cfg!(target_os = "windows") { let mut c = Command::new("rundll32"); c.arg("url.dll,FileProtocolHandler"); c } else { Command::new("xdg-open") };
    command.arg(target).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
    #[cfg(unix)]
    { use std::os::unix::process::CommandExt; command.process_group(0); }
    #[cfg(windows)]
    { use std::os::windows::process::CommandExt; command.creation_flags(0x00000008 | 0x00000200); }
    if let Ok(mut child) = command.spawn() {
        // Dropping Child does not reap it; a detached waiter must not hold the CLI open.
        std::thread::spawn(move || { let _ = child.wait(); });
    }
}
