//! Port of the `grok-mermaid` `labels.ts` helpers used by `source-box.ts`.
use std::sync::LazyLock;

use regex::Regex;

static CONTROLS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new("[\u{0}-\u{8}\u{b}\u{c}\u{e}-\u{1f}\u{7f}-\u{9f}]").expect("controls"));

pub fn strip_controls(src: &str) -> String {
    CONTROLS.replace_all(src, "").into_owned()
}

pub fn src_lines(src: &str) -> Vec<String> {
    let mut out: Vec<String> = src
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();
    if out.last().is_some_and(String::is_empty) {
        out.pop();
    }
    out
}
