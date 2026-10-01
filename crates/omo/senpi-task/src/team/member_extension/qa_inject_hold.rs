use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value};
use team_core::types::Message;

pub const QA_HOLD_AFTER_INJECT_ENV: &str = "SENPI_TASK_QA_HOLD_AFTER_INJECT";

/// Hook invoked after a message has been injected; blocks until released.
pub type QaAfterInjectHold = Box<dyn Fn(&Message) -> io::Result<()> + Send + Sync>;

const RELEASE_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub fn create_qa_after_inject_hold(env: &HashMap<String, String>) -> Option<QaAfterInjectHold> {
    let marker_path = env.get(QA_HOLD_AFTER_INJECT_ENV)?;
    if env.get("OMO_SENPI_QA").map(String::as_str) != Some("1") || marker_path.is_empty() {
        return None;
    }
    let marker_path = PathBuf::from(marker_path);
    Some(Box::new(move |message: &Message| {
        if let Some(parent) = marker_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let content = format!("{}\n", marker_json(message)?);
        write_private_file(&marker_path, content.as_bytes())?;
        let mut release = marker_path.clone().into_os_string();
        release.push(".release");
        wait_for_release(Path::new(&release))
    }))
}

fn marker_json(message: &Message) -> io::Result<String> {
    let value = serde_json::to_value(message).map_err(io::Error::other)?;
    let mut marker = Map::new();
    for key in ["messageId", "from", "to"] {
        if let Some(field) = value.get(key) {
            marker.insert(key.to_string(), field.clone());
        }
    }
    serde_json::to_string(&Value::Object(marker)).map_err(io::Error::other)
}

fn write_private_file(path: &Path, content: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(content)
}

fn wait_for_release(release_path: &Path) -> io::Result<()> {
    if release_path.exists() {
        return Ok(());
    }
    let watched_dir = release_path.parent().unwrap_or_else(|| Path::new("."));
    loop {
        // Mirrors fs.watch failing when the watched directory does not exist.
        fs::metadata(watched_dir)?;
        if release_path.exists() {
            return Ok(());
        }
        std::thread::sleep(RELEASE_POLL_INTERVAL);
    }
}
