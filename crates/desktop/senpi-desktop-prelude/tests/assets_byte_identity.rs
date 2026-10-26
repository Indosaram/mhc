use sha2::{Digest, Sha256};
use std::collections::HashMap;

use senpi_desktop_prelude::{COMPUTER_EXPORTS, ComputerPreludeAssets};

const FIXTURE_JSON: &str = include_str!("../test/assets.sha256.json");

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    format!("{:x}", hasher.finalize())
}

#[test]
fn keeps_prelude_assets_byte_identical_to_dev() {
    let fixture: HashMap<String, String> =
        serde_json::from_str(FIXTURE_JSON).expect("valid fixture JSON");

    assert_eq!(
        sha256_hex(ComputerPreludeAssets::javascript().as_bytes()),
        fixture["COMPUTER_PRELUDE_JAVASCRIPT"]
    );
    assert_eq!(
        sha256_hex(ComputerPreludeAssets::python().as_bytes()),
        fixture["COMPUTER_PRELUDE_PYTHON"]
    );
    assert_eq!(
        sha256_hex(ComputerPreludeAssets::declarations().as_bytes()),
        fixture["COMPUTER_DECLARATIONS"]
    );
    assert_eq!(
        sha256_hex(ComputerPreludeAssets::documentation().as_bytes()),
        fixture["COMPUTER_DOCUMENTATION"]
    );
    assert_eq!(
        sha256_hex(ComputerPreludeAssets::safety().as_bytes()),
        fixture["COMPUTER_SAFETY"]
    );
}

#[test]
fn still_declares_exactly_the_computer_export() {
    assert_eq!(ComputerPreludeAssets::exports(), &["computer"]);
    assert_eq!(COMPUTER_EXPORTS, &["computer"]);
}
