use std::collections::BTreeSet;
use senpi_desktop_prelude::{ComputerPreludeAssets, METHOD_ALLOWLIST};
use senpi_desktop_protocol::{DESKTOP_METHODS, ELEMENT_METHODS, WINDOW_METHODS};

fn is_host_control(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("resume")
        || lower.contains("stop")
        || lower.contains("start")
        || lower.contains("heartbeat")
        || lower.contains("session.open")
}

#[test]
fn equals_the_union_of_the_protocol_tier_tables() {
    let mut protocol_union = BTreeSet::new();
    for (name, _) in DESKTOP_METHODS {
        protocol_union.insert((*name).to_string());
    }
    for (name, _) in WINDOW_METHODS {
        protocol_union.insert((*name).to_string());
    }
    for (name, _) in ELEMENT_METHODS {
        protocol_union.insert((*name).to_string());
    }
    let expected: Vec<String> = protocol_union.into_iter().collect();

    let mut allowlist: Vec<String> = METHOD_ALLOWLIST.iter().map(|s| (*s).to_string()).collect();
    allowlist.sort();

    assert_eq!(allowlist, expected);
}

#[test]
fn matches_the_javascript_facade_helper_names() {
    let js = ComputerPreludeAssets::javascript();
    for method in METHOD_ALLOWLIST {
        assert!(js.contains(method), "JS facade missing method: {}", method);
    }
}

#[test]
fn matches_the_python_facade_helper_names() {
    let py = ComputerPreludeAssets::python();
    for method in METHOD_ALLOWLIST {
        let name = if *method == "raise" { "raise_" } else { *method };
        assert!(py.contains(name), "Python facade missing method: {}", name);
    }
}

#[test]
fn contains_no_host_only_control_a_model_could_reach() {
    let host_only: Vec<String> = METHOD_ALLOWLIST
        .iter()
        .filter(|n| is_host_control(n))
        .map(|s| (*s).to_string())
        .collect();
    assert!(host_only.is_empty(), "Allowlist exposed host controls: {:?}", host_only);
}
