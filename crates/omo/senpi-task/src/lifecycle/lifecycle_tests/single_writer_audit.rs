//! `lifecycle/single-writer-audit.test.ts`: only the lifecycle port (and the handle
//! implementations it drives) may invoke destruction. Scans this crate's non-test Rust sources.

use std::path::{Path, PathBuf};

/// Rust equivalents of the TS allowlist (`terminate.ts`/`handle.ts`/`start-cleanup.ts` live
/// under `runners/rpc/` in the Rust port too).
const INVOCATION_ALLOWLIST: &[&str] = &[
    "src/lifecycle/",
    "src/runners/rpc/terminate.rs",
    "src/runners/rpc/handle.rs",
    "src/runners/rpc/start_cleanup.rs",
    "src/runners/in_process/child_handle.rs",
    "src/manager/child_handle.rs",
];
const FORBIDDEN_PATTERNS: &[&str] = &[".dispose(", ".terminate(", "libc::kill(", ".kill("];

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn list_source_files(dir: &Path, files: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("read src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            list_source_files(&path, files);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            files.push(path);
        }
    }
}

fn is_test_source(rel: &str) -> bool {
    rel.contains("_tests") || rel.ends_with("tests.rs") || rel.contains("test_support")
}

fn is_allowlisted(rel: &str) -> bool {
    is_test_source(rel)
        || INVOCATION_ALLOWLIST
            .iter()
            .any(|allowed| rel.starts_with(allowed))
}

#[test]
fn no_destruction_invocations_outside_the_lifecycle_port() {
    let root = crate_root();
    let mut files = Vec::new();
    list_source_files(&root.join("src"), &mut files);
    let mut violations = Vec::new();
    for file in files {
        let rel = file
            .strip_prefix(&root)
            .expect("under crate root")
            .to_string_lossy()
            .replace('\\', "/");
        if is_allowlisted(&rel) {
            continue;
        }
        let source = std::fs::read_to_string(&file).expect("read source");
        for pattern in FORBIDDEN_PATTERNS {
            if source.contains(pattern) {
                violations.push(format!("{rel} :: {pattern}"));
            }
        }
    }
    assert_eq!(violations, Vec::<String>::new());
}

#[test]
fn lifecycle_port_heads_the_allowlist() {
    assert_eq!(INVOCATION_ALLOWLIST[0], "src/lifecycle/");
}
