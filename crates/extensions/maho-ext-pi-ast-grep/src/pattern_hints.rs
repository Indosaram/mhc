fn matches(expression: &str, text: &str) -> bool {
    regex::Regex::new(expression).expect("static upstream regex").is_match(text)
}

pub fn detect_regex_misuse(pattern: &str) -> Option<String> {
    let src = pattern.trim();
    let hint = if matches(r"\x5c[wWdDsSbB]", src) {
        r#"Hint: "\w", "\d", "\s", "\b" are regex escapes. ast-grep matches AST nodes, not text - use $VAR for identifiers, $$$ for node lists, or switch to grep for text search."#
    } else if matches(r"\[[a-zA-Z0-9]-[a-zA-Z0-9]\]", src) {
        r#"Hint: "[a-z]" and similar character classes are regex, not AST. Use $VAR to match any identifier, or switch to grep for text search."#
    } else if !src.contains('$') && matches(r"[A-Za-z0-9_]\.[*+]", src) {
        r#"Hint: ".*" and ".+" are regex wildcards. In ast-grep use $$$ for multiple AST nodes and $VAR for a single node. For text patterns, switch to grep."#
    } else if matches(r"^[-A-Za-z0-9_.*]+\|[-A-Za-z0-9_.*|]+$", src) {
        r#"Hint: "|" is regex alternation and does NOT work in ast-grep patterns. Options: (a) fire one ast_grep_search per alternative, or (b) switch to grep with a regex pattern like "foo|bar"."#
    } else { return None; };
    Some(hint.to_owned())
}

pub fn detect_language_specific_mistake(pattern: &str, language: &str) -> Option<String> {
    let src = pattern.trim();
    if language == "python" && (src.starts_with("class ") || src.starts_with("def ") || src.starts_with("async def ")) && src.ends_with(':') {
        return Some(format!("Hint: Remove trailing colon. Try: \"{}\"", &src[..src.len() - 1]));
    }
    if ["javascript", "typescript", "tsx"].contains(&language) && matches(r"(?i)^(export\s+)?(async\s+)?function\s+\$[A-Z_]+\s*$", src) {
        return Some("Hint: Function patterns need params and body. Try \"function $NAME($$$) { $$$ }\"".into());
    }
    if language == "go" && matches(r"(?i)^func\s+\$[A-Z_]+\s*$", src) {
        return Some("Hint: Go function patterns need params and body. Try \"func $NAME($$$) { $$$ }\"".into());
    }
    if language == "rust" && matches(r"(?i)^fn\s+\$[A-Z_]+\s*$", src) {
        return Some("Hint: Rust fn patterns need params and body. Try \"fn $NAME($$$) { $$$ }\"".into());
    }
    None
}

pub fn get_pattern_hint(pattern: &str, language: &str) -> Option<String> {
    detect_regex_misuse(pattern).or_else(|| detect_language_specific_mistake(pattern, language))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn escape_w() { assert!(detect_regex_misuse(r"\w+Mode").is_some()); }
    #[test] fn escape_d() { assert!(detect_regex_misuse(r"id\d+").is_some()); }
    #[test] fn escape_s() { assert!(detect_regex_misuse(r"name\s+value").is_some()); }
    #[test] fn escape_b() { assert!(detect_regex_misuse(r"\bword").is_some()); }
    #[test] fn character_class() { assert!(detect_regex_misuse("[a-z]+Mode").is_some()); }
    #[test] fn wildcard() { assert!(detect_regex_misuse("foo.*bar").is_some()); }
    #[test] fn alternation() { assert!(detect_regex_misuse("foo|bar").is_some()); }
    #[test] fn valid_ast() { for pattern in ["function $NAME($$$) { $$$ }", "console.log($$$)", "$A | $B"] { assert!(detect_regex_misuse(pattern).is_none()); } }
    #[test] fn python_class() { assert!(detect_language_specific_mistake("class Foo:", "python").is_some()); }
    #[test] fn python_def() { assert!(detect_language_specific_mistake("def foo():", "python").is_some()); }
    #[test] fn javascript_function() { assert!(detect_language_specific_mistake("function $NAME", "javascript").is_some()); }
    #[test] fn typescript_function() { assert!(detect_language_specific_mistake("function $NAME", "typescript").is_some()); }
    #[test] fn go_function() { assert!(detect_language_specific_mistake("func $NAME", "go").is_some()); }
    #[test] fn rust_function() { assert!(detect_language_specific_mistake("fn $NAME", "rust").is_some()); }
    #[test] fn valid_language_patterns() { for (pattern, language) in [("def $FUNC($$$)", "python"), ("function $NAME($$$) { $$$ }", "typescript"), ("func $NAME($$$) { $$$ }", "go"), ("fn $NAME($$$) { $$$ }", "rust")] { assert!(detect_language_specific_mistake(pattern, language).is_none()); } }
    #[test] fn regex_precedence() { assert_eq!(get_pattern_hint("foo|bar", "typescript"), detect_regex_misuse("foo|bar")); }
    #[test] fn language_fallback() { assert_eq!(get_pattern_hint("def $FUNC($$$):", "python"), detect_language_specific_mistake("def $FUNC($$$):", "python")); }
    #[test] fn clean_pattern() { assert!(get_pattern_hint("function $NAME($$$) { $$$ }", "typescript").is_none()); }
}
