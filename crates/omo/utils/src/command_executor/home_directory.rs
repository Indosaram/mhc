pub fn get_home_directory() -> String {
    ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .or_else(|| dirs::home_dir().map(|path| path.to_string_lossy().into_owned()))
        .unwrap_or_default()
}
