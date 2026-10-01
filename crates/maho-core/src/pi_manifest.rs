//! Port of senpi packages/coding-agent/src/core/pi-manifest.ts.

use std::path::Path;

use serde_json::Value;

use crate::text::strip_bom;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PiManifest {
    pub system: Option<bool>,
    pub extensions: Option<Vec<String>>,
    pub skills: Option<Vec<String>>,
    pub prompts: Option<Vec<String>>,
    pub themes: Option<Vec<String>>,
    pub hooks: Option<Vec<String>>,
}

const RESOURCE_FIELDS: [&str; 5] = ["extensions", "skills", "prompts", "themes", "hooks"];

fn string_array(value: Option<&Value>) -> Option<Vec<String>> {
    let array = value?.as_array()?;
    let mut entries = Vec::with_capacity(array.len());
    for entry in array {
        entries.push(entry.as_str()?.to_owned());
    }
    Some(entries)
}

pub fn read_pi_manifest(package_json_path: &str) -> Option<PiManifest> {
    let content = std::fs::read_to_string(Path::new(package_json_path)).ok()?;
    let package: Value = serde_json::from_str(strip_bom(&content)).ok()?;
    let pi = package.get("pi")?.as_object()?;

    let mut manifest = PiManifest::default();
    if let Some(system) = pi.get("system").and_then(Value::as_bool) {
        manifest.system = Some(system);
    }
    for field in RESOURCE_FIELDS {
        if let Some(entries) = string_array(pi.get(field)) {
            match field {
                "extensions" => manifest.extensions = Some(entries),
                "skills" => manifest.skills = Some(entries),
                "prompts" => manifest.prompts = Some(entries),
                "themes" => manifest.themes = Some(entries),
                "hooks" => manifest.hooks = Some(entries),
                _ => {}
            }
        }
    }
    Some(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &std::path::Path, name: &str, content: &str) -> String {
        let path = dir.join(name);
        std::fs::write(&path, content).expect("write");
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn reads_the_pi_manifest_fields() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(
            tmp.path(),
            "package.json",
            r#"{"name":"p","pi":{"system":true,"skills":["a","b"],"prompts":["c"]}}"#,
        );
        let manifest = read_pi_manifest(&path).expect("manifest");
        assert_eq!(manifest.system, Some(true));
        assert_eq!(manifest.skills, Some(vec!["a".into(), "b".into()]));
        assert_eq!(manifest.prompts, Some(vec!["c".into()]));
        assert!(manifest.themes.is_none());
    }

    #[test]
    fn a_missing_pi_block_is_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(tmp.path(), "package.json", r#"{"name":"p"}"#);
        assert!(read_pi_manifest(&path).is_none());
    }

    #[test]
    fn a_non_string_array_field_is_skipped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(tmp.path(), "package.json", r#"{"pi":{"skills":[1,2]}}"#);
        let manifest = read_pi_manifest(&path).expect("manifest");
        assert!(manifest.skills.is_none());
    }

    #[test]
    fn a_missing_or_malformed_file_is_none() {
        assert!(read_pi_manifest("/definitely/not/here/package.json").is_none());
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(tmp.path(), "package.json", "not json");
        assert!(read_pi_manifest(&path).is_none());
    }

    #[test]
    fn a_bom_is_stripped() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = write(tmp.path(), "package.json", &format!("\u{feff}{}", r#"{"pi":{"hooks":["h"]}}"#));
        assert_eq!(read_pi_manifest(&path).expect("manifest").hooks, Some(vec!["h".into()]));
    }
}
