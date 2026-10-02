use std::sync::LazyLock;
pub fn strip_ansi(value: &str) -> String {
    static ANSI: LazyLock<regex::Regex> = LazyLock::new(|| regex::Regex::new(r"(?:\x1b\][\s\S]*?(?:\x07|\x1b\\|\x{009c}))|[\x1b\x{009b}][\[\]()#;?]*(?:\d{1,4}(?:[;:]\d{0,4})*)?[\dA-PR-TZcf-nq-uy=><~]").expect("ANSI regex"));
    if !value.contains(['\x1b', '\u{009b}']) { return value.to_owned(); } ANSI.replace_all(value, "").into_owned()
}
