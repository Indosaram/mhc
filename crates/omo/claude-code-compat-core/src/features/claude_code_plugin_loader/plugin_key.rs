use std::path::Path;

pub fn derive_plugin_name_from_key(plugin_key: &str) -> String {
    let key = plugin_key.strip_prefix("npm:").unwrap_or(plugin_key);
    let separator = if key.starts_with('@') {
        key.find('/').and_then(|slash| key.get(slash..).and_then(|rest| rest.find('@').map(|at| slash + at)))
    } else { key.rfind('@') };
    let key = separator.filter(|at| *at > 0).and_then(|at| key.get(..at)).unwrap_or(key);
    if key.starts_with('@') && key.contains('/') { return key.to_owned(); }
    if key.contains('/') || key.contains('\\') {
        return Path::new(key).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    }
    key.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_file_url_falls_back_to_basename() {
        assert_eq!(derive_plugin_name_from_key("file://%zz/broken-plugin@1.0.0"), "broken-plugin");
    }
}
