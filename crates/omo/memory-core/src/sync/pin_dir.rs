pub fn sync_pin_dir(memory_root: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    memory_root.as_ref().join("sync-device")
}
