//! Port of senpi `anthropic-subscription/claude-cli-browser-capture.ts` (v2026.11.19-5).
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tokio::sync::mpsc;

/// Private browser shim, either capturing a URL or discarding it without opening a browser.
pub(crate) struct ClaudeCliBrowserCapture {
    watcher: Option<RecommendedWatcher>,
    changes: mpsc::Receiver<()>,
    dir: Option<TempDir>,
    helper_path: PathBuf,
    sink_path: PathBuf,
    consumed: usize,
}

fn helper_script(capture: bool, sink: &Path, windows: bool) -> String {
    if windows {
        if !capture {
            return "@exit /b 0\r\n".into();
        }
        let sink = sink.to_string_lossy().replace('%', "%%");
        return format!(
            "@echo off\r\ncd /d \"%SystemRoot%\"\r\nsetlocal DisableDelayedExpansion\r\nset \"u=%~1\"\r\nset \"sink={sink}\"\r\nsetlocal EnableDelayedExpansion\r\n>>\"!sink!\" echo(!u!\r\n"
        );
    }
    if !capture {
        return "#!/bin/sh\nexit 0\n".into();
    }
    let sink = sink.to_string_lossy().replace('\'', "'\\''");
    format!("#!/bin/sh\nprintf '%s\\n' \"$1\" >> '{sink}'\n")
}

impl ClaudeCliBrowserCapture {
    pub(crate) fn new(capture: bool) -> io::Result<Self> {
        let dir = tempfile::Builder::new()
            .prefix("maho-claude-browser-")
            .tempdir()?;
        let sink_path = dir.path().join("urls");
        let helper_path = dir.path().join(if cfg!(windows) {
            "browser.cmd"
        } else {
            "browser"
        });
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o700);
        }
        options
            .open(&helper_path)?
            .write_all(helper_script(capture, &sink_path, cfg!(windows)).as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&helper_path, fs::Permissions::from_mode(0o700))?;
        }
        let (sender, changes) = mpsc::channel(1);
        let watcher = if capture {
            let watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
                if event.is_ok_and(|event| {
                    matches!(
                        event.kind,
                        EventKind::Any | EventKind::Create(_) | EventKind::Modify(_)
                    )
                }) {
                    let _ = sender.try_send(());
                }
            })
            .and_then(|mut watcher| {
                watcher.watch(dir.path(), RecursiveMode::NonRecursive)?;
                Ok(watcher)
            });
            // If the platform watcher is unavailable, stdout and close still flush the sink.
            watcher.ok()
        } else {
            None
        };
        Ok(Self {
            watcher,
            changes,
            dir: Some(dir),
            helper_path,
            sink_path,
            consumed: 0,
        })
    }

    pub(crate) fn helper_path(&self) -> &Path {
        &self.helper_path
    }

    pub(crate) async fn changed(&mut self) {
        if self.changes.recv().await.is_none() {
            std::future::pending::<()>().await;
        }
    }

    pub(crate) fn flush(&mut self, mut on_url: impl FnMut(String)) {
        let Ok(bytes) = fs::read(&self.sink_path) else {
            return;
        };
        let Some(unread) = bytes.get(self.consumed..) else {
            return;
        };
        let Some(last_newline) = unread.iter().rposition(|byte| *byte == b'\n') else {
            return;
        };
        for line in String::from_utf8_lossy(&unread[..=last_newline]).lines() {
            let line = line.trim();
            if !line.is_empty() {
                on_url(line.to_owned());
            }
        }
        self.consumed += last_newline + 1;
    }
}

impl Drop for ClaudeCliBrowserCapture {
    fn drop(&mut self) {
        drop(self.watcher.take());
        if let Some(dir) = self.dir.take() {
            let path = dir.keep();
            let disposed = path.with_extension("disposed");
            // Revoke the sink path before removal so a late helper cannot recreate the file.
            let cleanup = if fs::rename(&path, &disposed).is_ok() {
                disposed
            } else {
                path
            };
            let _ = fs::remove_dir_all(cleanup);
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, process::Command};

    #[test]
    fn a_late_browser_helper_cannot_recreate_the_capture_sink() {
        let capture = ClaudeCliBrowserCapture::new(true).unwrap();
        let sink = capture.sink_path.clone();
        let replay_dir = tempfile::tempdir().unwrap();
        let replay = replay_dir.path().join("browser");
        fs::copy(capture.helper_path(), &replay).unwrap();
        fs::set_permissions(&replay, fs::Permissions::from_mode(0o700)).unwrap();
        drop(capture);

        let output = Command::new(replay)
            .env_clear()
            .arg("https://claude.ai/oauth/authorize?fixture=late")
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!sink.exists());
        assert!(!sink.parent().unwrap().with_extension("disposed").exists());
    }
}

#[cfg(all(test, windows))]
mod windows_tests {
    use super::*;
    use std::{os::windows::process::CommandExt, process::Command};

    #[test]
    fn cmd_helper_captures_query_metacharacters_verbatim() {
        let mut capture = ClaudeCliBrowserCapture::new(true).unwrap();
        let url = "https://claude.ai/oauth/authorize?scope=a+b&state=fixture";
        let status = Command::new("cmd.exe")
            .args(["/D", "/S", "/C"])
            .raw_arg(format!(
                "\"\"{}\" \"{url}\"\"",
                capture.helper_path().display()
            ))
            .current_dir(capture.helper_path().parent().unwrap())
            .status()
            .unwrap();
        assert!(status.success());
        let mut urls = Vec::new();
        capture.flush(|url| urls.push(url));
        assert_eq!(urls, [url]);
    }
}
