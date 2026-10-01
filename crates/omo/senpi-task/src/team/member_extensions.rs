//! Member extension list assembly.

use std::collections::HashSet;

/// Returns the member entry path followed by the inherited extensions, de-duplicated while keeping
/// first-seen order (JS `[...new Set([entryPath, ...inherited])]`).
pub fn assemble_member_extensions<S: AsRef<str>>(entry_path: &str, inherited_extensions: &[S]) -> Vec<String> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut assembled = Vec::new();
    for extension in std::iter::once(entry_path).chain(inherited_extensions.iter().map(AsRef::as_ref)) {
        if seen.insert(extension) {
            assembled.push(extension.to_string());
        }
    }
    assembled
}
