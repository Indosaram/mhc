//! Stable content-free extension display names.
use sha2::{Digest, Sha256};
use std::{collections::HashMap, path::Path};
pub fn prompt_size_extension_names(
    paths: &[String],
    package_name_of: impl Fn(&str) -> Option<String>,
) -> HashMap<String, String> {
    let mut names = HashMap::new();
    for path in paths {
        let p = Path::new(path);
        let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
        let name = if path.starts_with('<') {
            path.split_once(':')
                .map_or(path.as_str(), |(_, tail)| tail)
                .trim_end_matches('>')
                .to_owned()
        } else {
            package_name_of(path).unwrap_or_else(|| {
                if matches!(stem, "index" | "main") {
                    p.parent()
                        .and_then(Path::file_name)
                        .and_then(|s| s.to_str())
                        .unwrap_or_default()
                        .into()
                } else {
                    stem.into()
                }
            })
        };
        names.insert(path.clone(), name);
    }
    let mut counts = HashMap::new();
    for name in names.values() {
        *counts.entry(name.clone()).or_insert(0_usize) += 1;
    }
    for (path, name) in &mut names {
        if counts[name] > 1 {
            *name = format!(
                "{name}:{}",
                &hex::encode(Sha256::digest(path.as_bytes()))[..8]
            );
        }
    }
    names
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn disambiguates_only_collisions() {
        let paths = vec![
            "/a/index.ts".into(),
            "/b/a.ts".into(),
            "<builtin:goal>".into(),
        ];
        let names = prompt_size_extension_names(&paths, |_| None);
        assert!(names[&paths[0]].starts_with("a:"));
        assert_ne!(names[&paths[0]], names[&paths[1]]);
        assert_eq!(names[&paths[2]], "goal");
    }
}
