use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
fn sha(bytes: impl AsRef<[u8]>) -> String {
    format!("{:x}", Sha256::digest(bytes.as_ref()))
}
struct Identity {
    files: BTreeMap<String, String>,
    package_sha: String,
}
struct Fixture {
    root: PathBuf,
    identity: Identity,
}
fn files(directory: &Path, root: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(directory).expect("fixture invariant") {
        let entry = entry.expect("fixture invariant");
        let path = entry.path();
        if entry.file_type().expect("fixture invariant").is_dir() {
            files(&path, root, out);
        } else if entry.file_type().expect("fixture invariant").is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("fixture invariant")
                .to_str()
                .expect("fixture invariant")
                .to_owned();
            out.push(if relative.contains('/') {
                relative
            } else {
                format!("/{relative}")
            });
        }
    }
}
fn package_hash(root: &Path) -> String {
    let mut paths = Vec::new();
    files(root, root, &mut paths);
    paths.sort();
    let mut hash = Sha256::new();
    for path in paths {
        hash.update(path.as_bytes());
        hash.update(
            std::fs::read(root.join(path.trim_start_matches('/'))).expect("fixture invariant"),
        );
    }
    format!("{:x}", hash.finalize())
}
impl Fixture {
    fn new(label: &str) -> Self {
        let root = std::env::temp_dir()
            .join(format!("maho-tokenizer-{label}-{}", std::process::id()))
            .join("node_modules/gpt-tokenizer");
        let contents = BTreeMap::from([
            (
                "package.json",
                "{\"name\":\"gpt-tokenizer\",\"version\":\"4.0.0\",\"type\":\"module\"}",
            ),
            (
                "esm/encoding/o200k_base.js",
                "export const encode = text => Array.from(text);\n",
            ),
            ("esm/bpeRanks/o200k_base.js", "export default [];\n"),
        ]);
        for (name, bytes) in &contents {
            let path = root.join(name);
            std::fs::create_dir_all(path.parent().expect("fixture invariant"))
                .expect("fixture invariant");
            std::fs::write(path, bytes).expect("fixture invariant");
        }
        let identity = Identity {
            files: contents
                .iter()
                .map(|(k, v)| (k.to_string(), sha(v)))
                .collect(),
            package_sha: package_hash(&root),
        };
        Self { root, identity }
    }
    fn tokenize(
        &self,
        texts: &[&str],
        encode: impl Fn(&str) -> usize,
    ) -> Result<Vec<usize>, &'static str> {
        let pkg: serde_json::Value = serde_json::from_slice(
            &std::fs::read(self.root.join("package.json")).expect("fixture invariant"),
        )
        .expect("fixture invariant");
        if pkg["name"] != "gpt-tokenizer" || pkg["version"] != "4.0.0" {
            return Err("tokenizer_identity_drift");
        }
        for (path, hash) in &self.identity.files {
            if sha(std::fs::read(self.root.join(path)).expect("fixture invariant")) != *hash {
                return Err("tokenizer_identity_drift");
            }
        }
        if package_hash(&self.root) != self.identity.package_sha {
            return Err("tokenizer_identity_drift");
        }
        Ok(texts.iter().map(|text| encode(text)).collect())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(
            self.root
                .parent()
                .expect("fixture invariant")
                .parent()
                .expect("fixture invariant"),
        )
        .expect("fixture invariant");
    }
}
#[test]
fn verifies_installation_before_tokenizing() {
    let f = Fixture::new("valid");
    assert_eq!(
        f.tokenize(&["ab", ""], |text| text.chars().count()),
        Ok(vec![2, 0])
    );
}
fn changed(label: &str, path: &str, bytes: &str) {
    let f = Fixture::new(label);
    std::fs::write(f.root.join(path), bytes).expect("fixture invariant");
    assert_eq!(
        f.tokenize(&["ab"], |_| panic!("drift executed")),
        Err("tokenizer_identity_drift")
    );
}
#[test]
fn rejects_version_drift() {
    changed(
        "version",
        "package.json",
        "{\"name\":\"gpt-tokenizer\",\"version\":\"4.0.1\",\"type\":\"module\"}",
    );
}
#[test]
fn rejects_encoder_drift() {
    changed(
        "encoder",
        "esm/encoding/o200k_base.js",
        "throw new Error(\"drift_executed\"); export const encode = () => [];",
    );
}
#[test]
fn rejects_rank_drift() {
    changed(
        "ranks",
        "esm/bpeRanks/o200k_base.js",
        "export default [123];",
    );
}
#[test]
fn rejects_extra_implementation() {
    changed(
        "extra",
        "esm/other-implementation.js",
        "export const drift = true;",
    );
}
