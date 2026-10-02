use std::sync::LazyLock;
use regex::Regex;
static WHITESPACE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[\t\x0c\x0b \u{00a0}]+").expect("literal pattern"));
static BEFORE_NEWLINE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"[ \t]+\n").expect("literal pattern"));
static AFTER_NEWLINE:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\n[ \t]+").expect("literal pattern"));
static NEWLINES:LazyLock<Regex>=LazyLock::new(||Regex::new(r"\n{3,}").expect("literal pattern"));
pub fn normalize_plain_text(text:&str)->String {
    let spaces=WHITESPACE.replace_all(text," "); let before=BEFORE_NEWLINE.replace_all(&spaces,"\n"); let after=AFTER_NEWLINE.replace_all(&before,"\n"); NEWLINES.replace_all(&after,"\n\n").trim_matches(js_whitespace).into()
}
pub fn normalize_markdown(markdown:&str)->String {
    let normalized=markdown.replace("\r\n","\n").replace('\r',"\n"); let before=BEFORE_NEWLINE.replace_all(&normalized,"\n"); let after=AFTER_NEWLINE.replace_all(&before,"\n"); NEWLINES.replace_all(&after,"\n\n").trim_matches(js_whitespace).into()
}
fn js_whitespace(c:char)->bool { matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}') }
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn plain_normalization_keeps_two_newlines() { assert_eq!(normalize_plain_text("\u{feff} one\t two \n  three\n\n\n\n four \u{feff}"),"one two\nthree\n\nfour"); }
    #[test] fn markdown_normalizes_carriage_return_but_not_inline_space() { assert_eq!(normalize_markdown(" one   two\r\n  three\r\r\r four "),"one   two\nthree\n\nfour"); }
    #[test] fn javascript_does_not_trim_next_line_character() { assert_eq!(normalize_plain_text("\u{0085}x\u{0085}"),"\u{0085}x\u{0085}"); }
}
