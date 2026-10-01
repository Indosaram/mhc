#[derive(Debug, Clone)]
pub struct Open {
    pub char: char,
    pub line: usize,
    pub header_line: Option<usize>,
    pub target: bool,
    pub foldable: bool,
    pub protected: bool,
    pub signature: bool,
    pub interpolation: bool,
    pub control: bool,
    pub call: bool,
    pub value_parameters: bool,
    pub declaration: bool,
}
pub const EXPRESSION_KEYWORDS: &[&str] = &[
    "return",
    "throw",
    "yield",
    "await",
    "case",
    "typeof",
    "void",
    "delete",
    "in",
    "of",
    "instanceof",
];
pub const CONTROLS: &[&str] = &["if", "while", "for", "switch", "catch", "with"];
pub const SIGNATURE_DECLARATIONS: &[&str] = &[
    "type",
    "interface",
    "enum",
    "namespace",
    "module",
    "declare",
];
pub fn is_call_callee(previous: &str, before_word: &str) -> bool {
    let mut chars = previous.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
        && !["function", "async", "new"].contains(&previous)
        && !EXPRESSION_KEYWORDS.contains(&previous)
        && !CONTROLS.contains(&previous)
        && [".", "=", ":", "return", "await", "new"].contains(&before_word)
}
