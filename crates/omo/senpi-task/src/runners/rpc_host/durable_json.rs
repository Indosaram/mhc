//! Crash-safe small JSON files (`rpc-host/durable-json.ts`).
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};
pub const DURABLE_JSON_LOCK_WAIT_MS: u64 = 10_000;
pub fn read_text_if_present(path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}
pub async fn read_text_if_present_async(path: std::path::PathBuf) -> io::Result<Option<String>> {
    tokio::task::spawn_blocking(move || read_text_if_present(&path))
        .await
        .map_err(io::Error::other)?
}
pub fn write_text_durably(path: &Path, text: &str) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("missing parent directory"))?;
    fs::create_dir_all(parent)?;
    let staging = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&staging)?;
    file.write_all(text.as_bytes())?;
    file.sync_all()?;
    drop(file);
    if let Err(error) = fs::rename(&staging, path) {
        fs::remove_file(&staging)?;
        return Err(error);
    }
    #[cfg(not(windows))]
    {
        if let Err(error) = fs::File::open(parent)?.sync_all()
            && ![Some(22), Some(95)].contains(&error.raw_os_error()) {
                return Err(error);
            }
    }
    Ok(())
}
pub async fn write_text_durably_async(path: std::path::PathBuf, text: String) -> io::Result<()> {
    tokio::task::spawn_blocking(move || write_text_durably(&path, &text))
        .await
        .map_err(io::Error::other)?
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn async_write_replaces_whole_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("index.json");
        write_text_durably_async(path.clone(), "first".into())
            .await
            .unwrap();
        write_text_durably_async(path.clone(), "second".into())
            .await
            .unwrap();
        assert_eq!(
            read_text_if_present_async(path).await.unwrap().as_deref(),
            Some("second")
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
