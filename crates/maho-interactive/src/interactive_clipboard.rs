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
