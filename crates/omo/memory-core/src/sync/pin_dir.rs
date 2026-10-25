/// Per-account trust pins live beside memory repositories, not inside them.
pub fn sync_pin_dir(memory_root: impl AsRef<std::path::Path>) -> std::path::PathBuf {
    memory_root.as_ref().join("sync-device")
}

#[cfg(test)]
mod tests {
    #[test]
    fn pins_are_beside_repositories() {
        assert_eq!(
            super::sync_pin_dir(std::path::Path::new("memory")),
            std::path::PathBuf::from("memory/sync-device")
        );
    }
}
