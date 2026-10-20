use super::casefold::case_fold;
use unicode_normalization::UnicodeNormalization;
pub fn normalize_item_key(path: &str) -> Option<String> {
    if path.is_empty()
        || path.contains(['\0', '\\'])
        || path.starts_with('/')
        || (path.len() >= 2
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':')
    {
        return None;
    }
    let normalized: String = path.nfc().collect();
    if normalized.split('/').any(|s| matches!(s, "" | "." | "..")) {
        return None;
    }
    Some(case_fold(&normalized).nfc().collect())
}
