//! `runners/rpc/parent-extensions.test.ts`.

use crate::runners::rpc::parent_extensions::parse_extension_entries;

fn argv(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| (*item).to_string()).collect()
}

#[test]
fn every_extension_entry_is_collected_in_order() {
    let entries = parse_extension_entries(&argv(&[
        "node",
        "senpi",
        "-e",
        "/tmp/a.ts",
        "--mode",
        "json",
        "--extension",
        "/tmp/b.ts",
        "-p",
        "go",
    ]));
    assert_eq!(entries, vec!["/tmp/a.ts", "/tmp/b.ts"]);
}

#[test]
fn no_extension_flags_yield_nothing() {
    assert!(parse_extension_entries(&argv(&["node", "senpi", "--mode", "rpc"])).is_empty());
}

#[test]
fn dangling_flag_is_ignored() {
    assert!(parse_extension_entries(&argv(&["node", "senpi", "-e"])).is_empty());
}
