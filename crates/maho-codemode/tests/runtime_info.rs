use maho_codemode::extension::runtime_info::*;

#[test]
fn bun_marker_selects_runtime_identity() {
    assert_eq!(js_runtime_info("24.1.0", Some("1.4.0"), "/bin/bun").name, "bun");
    assert_eq!(js_runtime_info("24.1.0", Some(""), "/bin/node").name, "node");
    assert_eq!(js_runtime_info("24.1.0", None, "/bin/node").version, "24.1.0");
}

#[test]
fn bun_identity_preserves_version_and_path() {
    let runtime = js_runtime_info("26.7.0", Some("1.4.0"), "/opt/bun/bin/bun");
    assert_eq!(runtime.version, "1.4.0");
    assert_eq!(runtime.path.as_deref(), Some("/opt/bun/bin/bun"));
}

#[test]
fn bun_runtime_label_uses_bun_version() {
    assert_eq!(js_runtime_label("26.7.0", Some("1.4.0")), "bun 1.4.0");
}

#[test]
fn node_runtime_label_uses_node_version() {
    assert_eq!(js_runtime_label("26.7.0", None), "node 26.7.0");
}

#[test]
fn probe_without_runtime_on_path_falls_back_to_node_identity() {
    let runtime = probe_js_runtime_info(&std::collections::HashMap::from([("PATH".to_owned(), String::new())]));
    assert_eq!(runtime.name, "node");
    assert_eq!(runtime.version, "");
}
