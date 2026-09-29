use std::sync::LazyLock;

use fancy_regex::Regex;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintSeverity {
    Warn,
    Reject,
    AlwaysReject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub code: &'static str,
    pub severity: HintSeverity,
    pub message: String,
}

#[derive(Debug, Clone, Default)]
pub struct ValidationOpts<'a> {
    pub force: bool,
    pub paths: Option<&'a Value>,
    pub limit: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidationResult {
    pub ok: bool,
    pub rejected: bool,
    pub code: Option<&'static str>,
    pub hints: Vec<Hint>,
}

impl ValidationResult {
    fn rejected(code: &'static str, hints: Vec<Hint>) -> Self {
        Self {
            ok: false,
            rejected: true,
            code: Some(code),
            hints,
        }
    }

    fn accepted(hints: Vec<Hint>) -> Self {
        Self {
            ok: true,
            rejected: false,
            code: None,
            hints,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MetavarSet {
    pub single: Vec<String>,
    pub multi: Vec<String>,
}

pub const LANGUAGES: [&str; 25] = [
    "bash",
    "c",
    "cpp",
    "csharp",
    "css",
    "elixir",
    "go",
    "haskell",
    "html",
    "java",
    "javascript",
    "json",
    "kotlin",
    "lua",
    "nix",
    "php",
    "python",
    "ruby",
    "rust",
    "scala",
    "solidity",
    "swift",
    "typescript",
    "tsx",
    "yaml",
];

const LANG_ALIASES: [(&str, &str); 19] = [
    ("js", "javascript"),
    ("jsx", "javascript"),
    ("ts", "typescript"),
    ("py", "python"),
    ("py3", "python"),
    ("rb", "ruby"),
    ("rs", "rust"),
    ("kt", "kotlin"),
    ("ex", "elixir"),
    ("hs", "haskell"),
    ("sh", "bash"),
    ("zsh", "bash"),
    ("cc", "cpp"),
    ("c++", "cpp"),
    ("cxx", "cpp"),
    ("cs", "csharp"),
    ("yml", "yaml"),
    ("sol", "solidity"),
    ("golang", "go"),
];

fn regex(pattern: &str) -> Regex {
    Regex::new(pattern).expect("static pattern-hint regex compiles")
}

static RE_BACKSLASH: LazyLock<Regex> = LazyLock::new(|| regex(r"\\w|\\d|\\s|\\b"));
static RE_DOT_STAR: LazyLock<Regex> = LazyLock::new(|| regex(r"(?<!\$)\.\*|(?<!\$)\.\+"));
static RE_DOUBLE_DOLLAR: LazyLock<Regex> = LazyLock::new(|| regex(r"(?<!\$)\$\$(?!\$)[A-Za-z_]"));
static RE_ANY_METAVAR: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?<!\$)\$(?!\$)([A-Za-z_][A-Za-z0-9_]*)"));
static RE_UPPER_NAME: LazyLock<Regex> = LazyLock::new(|| regex(r"^[A-Z][A-Z0-9_]*$"));
static RE_PY_TRAILING_COLON: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^\s*(?:def|class)\s+\$?[A-Za-z0-9_]+[^:]*:\s*$"));
static RE_JS_INCOMPLETE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^\s*(?:async\s+)?function\s+\$?[A-Za-z0-9_]+(?:\([^)]*\))?\s*$"));
static RE_GO_INCOMPLETE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^\s*func\s+\$?[A-Za-z0-9_]+(?:\([^)]*\))?\s*$"));
static RE_RUST_INCOMPLETE: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?m)^\s*fn\s+\$?[A-Za-z0-9_]+(?:\([^)]*\))?\s*$"));
static RE_CHAR_CLASS_SHAPE: LazyLock<Regex> = LazyLock::new(|| regex(r"^\[\^?[^\]]+\]$"));
static RE_ALNUM_RANGE: LazyLock<Regex> = LazyLock::new(|| regex(r"[a-zA-Z0-9]-[a-zA-Z0-9]"));
static RE_QUOTED: LazyLock<Regex> = LazyLock::new(|| regex(r#"'[^']*'|"[^"]*"|`[^`]*`"#));
static RE_ALTERNATION: LazyLock<Regex> = LazyLock::new(|| {
    regex(
        r"(?:[A-Za-z0-9_]|\$[A-Za-z0-9_]+|[A-Za-z0-9_]+\(\))\s*\|\s*(?:[A-Za-z0-9_]|\$[A-Za-z0-9_]+|[A-Za-z0-9_]+\(\))",
    )
});
static RE_MULTI_METAVAR: LazyLock<Regex> = LazyLock::new(|| regex(r"\$\$\$([A-Z][A-Z0-9_]*)"));
static RE_SINGLE_METAVAR: LazyLock<Regex> =
    LazyLock::new(|| regex(r"(?<!\$)\$(?!\$)([A-Z][A-Z0-9_]*)"));

fn matches(re: &Regex, text: &str) -> bool {
    re.is_match(text).unwrap_or(false)
}

fn is_regex_char_class(pattern: &str) -> bool {
    let trimmed = pattern.trim();
    if !matches(&RE_CHAR_CLASS_SHAPE, trimmed) {
        return false;
    }
    let without_open = trimmed
        .strip_prefix("[^")
        .or_else(|| trimmed.strip_prefix('['));
    let inner = without_open
        .unwrap_or(trimmed)
        .strip_suffix(']')
        .unwrap_or_default();
    if inner.contains(',') {
        return false;
    }
    matches(&RE_ALNUM_RANGE, inner) || trimmed.starts_with("[^") || inner.contains(['_', '.', ' '])
}

pub fn normalize_language(language: &str) -> Option<&'static str> {
    let lower = language.to_lowercase();
    let canonical = LANG_ALIASES
        .iter()
        .find(|(alias, _)| *alias == lower)
        .map_or(lower.as_str(), |(_, canonical)| canonical);
    LANGUAGES.iter().copied().find(|name| *name == canonical)
}

fn find_alternation(pattern: &str) -> bool {
    let stripped = RE_QUOTED.replace_all(pattern, "");
    if stripped.contains("||") {
        return false;
    }
    matches(&RE_ALTERNATION, &stripped)
}

fn is_valid_paths(paths: &Value) -> bool {
    match paths {
        Value::Array(items) => {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.as_str().is_some_and(|path| !path.is_empty()))
        }
        _ => false,
    }
}

fn is_valid_limit(limit: f64) -> bool {
    limit.is_finite() && limit.fract() == 0.0 && limit > 0.0
}

fn captures(re: &Regex, text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for name in re
        .captures_iter(text)
        .filter_map(Result::ok)
        .filter_map(|captures| captures.get(1).map(|name| name.as_str().to_owned()))
    {
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

pub fn extract_metavars(text: &str) -> MetavarSet {
    MetavarSet {
        single: captures(&RE_SINGLE_METAVAR, text),
        multi: captures(&RE_MULTI_METAVAR, text),
    }
}

fn hint(code: &'static str, severity: HintSeverity, message: impl Into<String>) -> Hint {
    Hint {
        code,
        severity,
        message: message.into(),
    }
}

fn first_code(hints: &[Hint], severity: HintSeverity) -> Option<&'static str> {
    hints
        .iter()
        .find(|hint| hint.severity == severity)
        .map(|hint| hint.code)
}

pub fn validate_pattern_hints(
    pattern: &str,
    language: &str,
    opts: &ValidationOpts<'_>,
) -> ValidationResult {
    use HintSeverity::AlwaysReject;
    use HintSeverity::Reject;
    use HintSeverity::Warn;

    let mut hints = Vec::new();
    if pattern.trim().is_empty() {
        hints.push(hint("PATTERN_EMPTY", AlwaysReject, "Pattern is empty."));
    }
    let canonical = normalize_language(language);
    if canonical.is_none() {
        hints.push(hint(
            "LANGUAGE_UNSUPPORTED",
            AlwaysReject,
            format!(
                "Language '{language}' is not supported. Use one of the 25 ast-grep languages."
            ),
        ));
    }
    if opts.paths.is_some_and(|paths| !is_valid_paths(paths)) {
        hints.push(hint(
            "INVALID_PATH",
            AlwaysReject,
            "Paths must be a non-empty array of non-empty strings.",
        ));
    }
    if opts.limit.is_some_and(|limit| !is_valid_limit(limit)) {
        hints.push(hint(
            "INVALID_LIMIT",
            AlwaysReject,
            "Limit must be a positive finite integer.",
        ));
    }
    if let Some(code) = first_code(&hints, AlwaysReject) {
        return ValidationResult::rejected(code, hints);
    }

    if matches(&RE_BACKSLASH, pattern) {
        hints.push(hint(
            "REGEX_BACKSLASH_ESCAPE",
            Reject,
            "Backslash escapes (\\w, \\d, \\s, \\b) are regex, not ast-grep. Use $VAR for identifiers.",
        ));
    }
    if matches(&RE_DOT_STAR, pattern) {
        hints.push(hint(
            "REGEX_DOT_STAR",
            Reject,
            "'.*' and '.+' are regex wildcards. Use $$$ for multiple nodes or $VAR for one.",
        ));
    }
    if is_regex_char_class(pattern) {
        hints.push(hint(
            "REGEX_CHAR_CLASS",
            Reject,
            "Character classes like [a-z] are regex syntax. ast-grep has no AST equivalent.",
        ));
    }
    let incomplete = match canonical {
        Some("python") if matches(&RE_PY_TRAILING_COLON, pattern) => Some(
            "Python pattern has trailing ':'. Drop the colon: 'def $FUNC($$$)' or 'class $C($$$)'.",
        ),
        Some("javascript" | "typescript" | "tsx") if matches(&RE_JS_INCOMPLETE, pattern) => Some(
            "JS/TS function pattern is incomplete. Add params and body: 'function $NAME($$$) { $$$ }'.",
        ),
        Some("go") if matches(&RE_GO_INCOMPLETE, pattern) => Some(
            "Go function pattern is incomplete. Add params and body: 'func $NAME($$$) { $$$ }'.",
        ),
        Some("rust") if matches(&RE_RUST_INCOMPLETE, pattern) => Some(
            "Rust fn pattern is incomplete. Add params and body: 'fn $NAME($$$) -> $RET { $$$ }'.",
        ),
        _ => None,
    };
    if let Some(message) = incomplete {
        hints.push(hint("PATTERN_INCOMPLETE_FORM", Reject, message));
    }
    if matches(&RE_DOUBLE_DOLLAR, pattern) {
        hints.push(hint(
            "METAVAR_DOUBLE_DOLLAR",
            Reject,
            "$$NAME is invalid. Use $$$NAME for multi-node capture or $NAME for single.",
        ));
    }
    let invalid_name = RE_ANY_METAVAR
        .captures_iter(pattern)
        .filter_map(Result::ok)
        .filter_map(|captures| captures.get(1).map(|name| name.as_str().to_owned()))
        .find(|name| name != "_" && !matches(&RE_UPPER_NAME, name));
    if let Some(name) = invalid_name {
        let upper = name.to_uppercase();
        hints.push(hint(
            "INVALID_METAVAR_NAME",
            Reject,
            format!(
                "Metavariable name ${name} must be UPPERCASE (e.g. ${upper}) or use $_ for wildcard."
            ),
        ));
    }
    if find_alternation(pattern) {
        hints.push(hint(
            "BARE_ALTERNATION",
            Warn,
            "Literal '|' may be a TS union or bitwise-or. If regex alternation, use separate calls.",
        ));
    }

    if first_code(&hints, Reject).is_some() && !opts.force {
        return ValidationResult::rejected("PATTERN_HINT_REJECTED", hints);
    }
    ValidationResult::accepted(hints)
}

pub fn validate_rewrite_hints(
    pattern: &str,
    rewrite: &str,
    language: &str,
    opts: &ValidationOpts<'_>,
) -> ValidationResult {
    let pattern_result = validate_pattern_hints(pattern, language, opts);
    if pattern_result.rejected && pattern_result.code != Some("PATTERN_HINT_REJECTED") {
        return pattern_result;
    }
    let mut hints = pattern_result.hints.clone();
    let pm = extract_metavars(pattern);
    let rm = extract_metavars(rewrite);
    let pattern_names: Vec<&String> = pm.single.iter().chain(&pm.multi).collect();
    let mut rewrite_names: Vec<&String> = Vec::new();
    for name in rm.single.iter().chain(&rm.multi) {
        if !rewrite_names.contains(&name) {
            rewrite_names.push(name);
        }
    }

    if let Some(name) = rewrite_names
        .iter()
        .find(|name| !pattern_names.contains(*name))
    {
        hints.push(hint(
            "REWRITE_UNBOUND_METAVARIABLE",
            HintSeverity::AlwaysReject,
            format!("Rewrite uses metavariable ${name} not captured by pattern."),
        ));
    }
    let mismatched = rewrite_names.iter().find(|name| {
        if !pattern_names.contains(*name) {
            return false;
        }
        let p_single = pm.single.contains(**name);
        let p_multi = pm.multi.contains(**name);
        let r_single = rm.single.contains(**name);
        let r_multi = rm.multi.contains(**name);
        (p_single && !p_multi && r_multi && !r_single)
            || (p_multi && !p_single && r_single && !r_multi)
    });
    if let Some(name) = mismatched {
        hints.push(hint(
            "REWRITE_CARDINALITY_MISMATCH",
            HintSeverity::AlwaysReject,
            format!("Metavariable {name} cardinality mismatch between pattern and rewrite."),
        ));
    }

    if let Some(code) = first_code(&hints, HintSeverity::AlwaysReject) {
        return ValidationResult::rejected(code, hints);
    }
    if pattern_result.rejected && !opts.force {
        return ValidationResult::rejected("PATTERN_HINT_REJECTED", hints);
    }
    ValidationResult::accepted(hints)
}
