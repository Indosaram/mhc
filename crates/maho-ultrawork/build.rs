//! Build-time gate on the shipped contract: size ceiling and forbidden tokens.

use std::fs;
use std::path::Path;

/// Measured size of the authored core contract (13,894 bytes) plus the 10% ceiling.
const CORE_CAP_BYTES: usize = 15_283;
/// Per-reference budget for the on-demand modules.
const REFERENCE_CAP_BYTES: usize = 6_000;
const FORBIDDEN_TOKENS: [&str; 3] = ["HOP to", "omo-agent-toolkit", "script/qa/"];

fn check(path: &Path, cap: usize) {
    let text = fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
    assert!(
        text.len() <= cap,
        "{} is {} bytes, over the {cap}-byte cap",
        path.display(),
        text.len()
    );
    for token in FORBIDDEN_TOKENS {
        assert!(
            !text.contains(token),
            "{} contains forbidden token {token:?}",
            path.display()
        );
    }
}

fn main() {
    let contract = Path::new("contract");
    println!("cargo:rerun-if-changed=contract");
    println!("cargo:rerun-if-changed=build.rs");
    check(&contract.join("core.md"), CORE_CAP_BYTES);
    let references = fs::read_dir(contract.join("references"))
        .unwrap_or_else(|error| panic!("cannot list contract/references: {error}"));
    for entry in references {
        let path = entry
            .unwrap_or_else(|error| panic!("cannot read a references entry: {error}"))
            .path();
        check(&path, REFERENCE_CAP_BYTES);
    }
}
