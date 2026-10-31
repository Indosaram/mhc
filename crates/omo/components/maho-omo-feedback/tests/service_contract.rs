//! Pins the service-contract fixture snapshot (omo v5.1.52 `fixtures/service-contract`) and checks the
//! client maps exactly the service's closed error-code list.

use maho_omo_feedback::submit::service_refusal_codes;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

fn fixture(name: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/service-contract")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

#[test]
fn fixture_snapshot_matches_its_provenance_receipts() {
    let provenance: Value = serde_json::from_str(&fixture("provenance.json")).expect("provenance");
    for (file, source) in [
        ("reports.ts", "reports.ts"),
        ("schema.ts", "schema.ts"),
        ("errors.ts", "errors.ts.txt"),
    ] {
        let expected = provenance["files"][file].as_str().expect("receipt");
        let actual = format!("{:x}", Sha256::digest(fixture(source).as_bytes()));
        assert_eq!(actual, expected, "{file}");
    }
}

#[test]
fn every_service_error_code_has_a_typed_refusal_and_nothing_else_does() {
    let source = fixture("errors.ts.txt");
    let start = source
        .find("export const ERROR_CODES = [")
        .expect("list start");
    let list = &source[start
        ..source[start..]
            .find("] as const")
            .map(|end| start + end)
            .expect("list end")];
    let mut contract: Vec<String> = Vec::new();
    for line in list.lines() {
        let trimmed = line.trim();
        if let Some(code) = trimmed
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix("\","))
        {
            contract.push(code.to_owned());
        }
    }
    contract.sort();
    let mut mapped: Vec<String> = service_refusal_codes()
        .into_iter()
        .map(str::to_owned)
        .collect();
    mapped.sort();
    assert_eq!(mapped, contract);
}
