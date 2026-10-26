use std::collections::BTreeSet;
use senpi_desktop_protocol::{DESKTOP_METHODS, ELEMENT_METHODS, WINDOW_METHODS};

const CHAIN_METHOD_SNAPSHOT: &[&str] = &[
    "actions",
    "attributes",
    "ax",
    "bounds",
    "capabilities",
    "children",
    "click",
    "clipboard.read",
    "clipboard.write",
    "displays",
    "doubleClick",
    "drag",
    "elementAt",
    "find",
    "focus",
    "focusedElement",
    "focusedWindow",
    "move",
    "parent",
    "perform",
    "press",
    "raise",
    "ref",
    "screenshot",
    "scroll",
    "setValue",
    "type",
    "value",
    "window",
    "windows",
];

fn chain_methods() -> Vec<String> {
    let mut names = BTreeSet::new();
    for (name, _) in DESKTOP_METHODS {
        names.insert((*name).to_string());
    }
    for (name, _) in WINDOW_METHODS {
        names.insert((*name).to_string());
    }
    for (name, _) in ELEMENT_METHODS {
        names.insert((*name).to_string());
    }
    names.into_iter().collect()
}

fn is_host_control(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("resume")
        || lower.contains("stop")
        || lower.contains("start")
        || lower.contains("heartbeat")
        || lower.contains("session.open")
}

#[test]
fn equals_the_snapshot_when_every_table_is_unioned() {
    let expected: Vec<String> = CHAIN_METHOD_SNAPSHOT.iter().map(|s| (*s).to_string()).collect();
    assert_eq!(chain_methods(), expected);
}

#[test]
fn exposes_no_stop_path_resume_or_session_control_to_a_chain() {
    let exposed: Vec<String> = chain_methods().into_iter().filter(|n| is_host_control(n)).collect();
    assert!(exposed.is_empty(), "Exposed host controls: {:?}", exposed);
}
