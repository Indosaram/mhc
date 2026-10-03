use maho_codemode::{extension::runtime_info::*, interpreters::detect::InterpreterDetection, tool::types::EvalLanguage};

#[test]
fn bun_marker_selects_runtime_identity() {
    assert_eq!(js_runtime_info("24.1.0", Some("1.4.0"), "/bin/bun").name, "bun");
    assert_eq!(js_runtime_info("24.1.0", Some(""), "/bin/node").name, "node");
    assert_eq!(js_runtime_info("24.1.0", None, "/bin/node").version, "24.1.0");
}

#[test]
fn availability_prefers_resolved_path_and_omits_missing() {
    let availability = [(EvalLanguage::Py, InterpreterDetection::Detected { path: "py -3".into(), version: "3.12.4".into(), resolved_path: Some("/bin/python".into()) }), (EvalLanguage::Rb, InterpreterDetection::Unavailable)];
    let runtimes = runtimes_from_availability(&availability, js_runtime_info("24.1.0", None, "/bin/node"));
    assert_eq!(runtimes.len(), 2);
    assert_eq!(runtimes[1].1.path.as_deref(), Some("/bin/python"));
    assert_eq!(runtimes[1].1.name, "python");
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
fn availability_falls_back_to_probe_command() {
    let availability = [(EvalLanguage::Rb, InterpreterDetection::Detected { path: "ruby".into(), version: "3.3.6".into(), resolved_path: None })];
    let js = js_runtime_info("26.7.0", None, "/bin/node");
    let runtimes = runtimes_from_availability(&availability, js.clone());
    assert_eq!(runtimes[0].1, js);
    assert_eq!(runtimes[1].1.path.as_deref(), Some("ruby"));
    assert_eq!(runtimes[1].1.name, "ruby");
    assert_eq!(runtimes.len(), 2);
}
