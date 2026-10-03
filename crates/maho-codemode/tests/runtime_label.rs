use maho_codemode::tool::{runtime_label::*, types::EvalLanguage};

#[test]
fn contracts_home_with_separator_boundary() {
    assert_eq!(minify_path("/Users/dev/bin/julia", "/Users/dev"), "~/bin/julia");
    assert_eq!(minify_path("/Users/developer", "/Users/dev"), "/Users/developer");
    assert_eq!(minify_path("/Users/dev", "/Users/dev"), "~");
}
#[test]
fn middle_path_retains_tail_and_head() {
    let minified = minify_path("/opt/homebrew/Cellar/node-runtime/26.7.0_1/libexec/bin/node", "/Users/dev");
    assert!(minified.starts_with("/opt/"));
    assert!(minified.ends_with("/bin/node"));
    assert!(minified.chars().count() <= 40);
}
#[test]
fn oversized_segment_caps_codepoints() {
    let minified = minify_path(&format!("/tmp/{}", "가".repeat(80)), "");
    assert_eq!(minified.chars().count(), 40);
    assert!(minified.ends_with(&"가".repeat(39)));
}
#[test]
fn windows_home_contracts() {
    assert_eq!(minify_path(r"C:\Users\dev\bin\ruby", r"C:\Users\dev"), r"~\bin\ruby");
}
#[test]
fn runtime_badge_distinguishes_javascript_hosts() {
    assert!(format_runtime_badge(EvalLanguage::Js, "bun", "1.4.0", None, "").starts_with("bun "));
    assert_eq!(format_runtime_badge(EvalLanguage::Py, "python", "3.12.4", None, ""), "3.12.4");
}
