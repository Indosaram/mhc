use super::theme::{Theme, ThemeColor};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};
use syntect::{
    parsing::{ParseState, ScopeStack, SyntaxSet},
    util::LinesWithEndings,
};
/// senpi's `utils/syntax-highlight.ts` registers 20 highlight.js languages eagerly, and
/// `supportsLanguage` accepts only those names and their aliases (case-insensitive). Each maps to
/// its highlight.js name and the two-face (bat) grammar that stands in for it here.
fn eager_language(lang: &str) -> Option<(&'static str, &'static str)> {
    let language = match lang.to_ascii_lowercase().as_str() {
        "python" | "py" | "gyp" | "ipython" => ("python", "Python"),
        "java" | "jsp" => ("java", "Java"),
        "go" | "golang" => ("go", "Go"),
        "javascript" | "js" | "jsx" | "mjs" | "cjs" => ("javascript", "JavaScript"),
        "json" => ("json", "JSON"),
        "cpp" | "cc" | "c++" | "h++" | "hpp" | "hh" | "hxx" | "cxx" => ("cpp", "C++"),
        "typescript" | "ts" | "tsx" | "mts" | "cts" => ("typescript", "TypeScript"),
        "php" => ("php", "PHP Source"),
        "ruby" | "rb" | "gemspec" | "podspec" | "thor" | "irb" => ("ruby", "Ruby"),
        "c" | "h" => ("c", "C"),
        "csharp" | "cs" | "c#" => ("csharp", "C#"),
        "nix" | "nixos" => ("nix", "Nix"),
        "bash" | "sh" | "zsh" => ("bash", "Bourne Again Shell (bash)"),
        "rust" | "rs" => ("rust", "Rust"),
        "scala" => ("scala", "Scala"),
        "kotlin" | "kt" | "kts" | "ktm" | "ktx" => ("kotlin", "Kotlin"),
        "swift" => ("swift", "Swift"),
        "dart" => ("dart", "Dart"),
        "groovy" => ("groovy", "Groovy"),
        "perl" | "pl" | "pm" => ("perl", "Perl"),
        "lua" | "pluto" => ("lua", "Lua"),
        _ => return None,
    };
    Some(language)
}
/// senpi `loadAllHighlightLanguages`: after it runs, every language the full highlight.js bundle
/// knows is highlighted, not just the eager twenty. Here the full bundle is two-face's grammar set,
/// looked up by name or file-extension token.
static ALL_LANGUAGES_LOADED: AtomicBool = AtomicBool::new(false);
pub fn load_all_highlight_languages() {
    ALL_LANGUAGES_LOADED.store(true, Ordering::Relaxed);
}
/// senpi `supportsLanguage`.
pub fn supports_language(name: &str) -> bool {
    resolve_language(name, ALL_LANGUAGES_LOADED.load(Ordering::Relaxed)).is_some()
}
/// The highlight.js name and grammar for `lang`: an eager language always, any grammar two-face
/// carries once all languages are loaded.
fn resolve_language(
    lang: &str,
    all_loaded: bool,
) -> Option<(String, &'static syntect::parsing::SyntaxReference)> {
    let syntax_set = syntax_set();
    if let Some((name, grammar)) = eager_language(lang) {
        return syntax_set
            .find_syntax_by_name(grammar)
            .map(|syntax| (name.to_owned(), syntax));
    }
    if !all_loaded || lang.is_empty() {
        return None;
    }
    let syntax = syntax_set.find_syntax_by_token(lang)?;
    Some((lang.to_ascii_lowercase(), syntax))
}
fn syntax_set() -> &'static SyntaxSet {
    static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
    SYNTAX_SET.get_or_init(two_face::syntax::extra_newlines)
}
const PRIMITIVE_TYPES: &[&str] = &[
    "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
    "f32", "f64", "bool", "char", "str", "int", "long", "short", "float", "double", "void",
    "unsigned", "signed", "byte", "boolean", "string", "auto", "size_t", "wchar_t", "uint",
    "rune", "error",
];
/// highlight.js `meta` spans (attributes, decorators, annotations, preprocessor lines).
fn is_meta(scope: &str) -> bool {
    ["meta.annotation", "meta.decorator", "meta.attribute", "meta.preprocessor"]
        .iter()
        .any(|prefix| scope.starts_with(prefix))
}
/// Maps one TextMate scope onto the highlight.js class it stands for. `None` lets the enclosing
/// scope decide.
fn scope_class(scope: &str, text: &str, lang: &str) -> Option<&'static str> {
    let word = text.trim();
    let is_word = word.chars().any(char::is_alphanumeric);
    let starts = |prefix: &str| scope.starts_with(prefix);
    if starts("markup.inserted") {
        Some("addition")
    } else if starts("markup.deleted") {
        Some("deletion")
    } else if lang == "json" && starts("punctuation.definition.string") {
        Some("string")
    } else if lang == "json" && starts("punctuation") {
        Some("punctuation")
    } else if lang == "json" && starts("string.quoted.double.json") && scope.contains("key") {
        Some("attr")
    } else if lang == "json" && starts("constant.language") {
        Some("keyword")
    } else if starts("comment") {
        Some("comment")
    } else if starts("string") || starts("storage.type.string") {
        Some("string")
    } else if starts("constant.numeric") || starts("constant.language") || starts("storage.type.numeric") {
        Some("number")
    } else if starts("keyword.operator") {
        // Word operators (`as`, `new`, `typeof`) are keywords in highlight.js; symbolic operators
        // carry the `operator` class only in the grammars that emit it.
        if is_word {
            Some("keyword")
        } else if matches!(lang, "swift" | "nix") {
            Some("operator")
        } else {
            None
        }
    } else if starts("keyword") || starts("storage.modifier") {
        Some("keyword")
    } else if !is_word && starts("storage.type") {
        None
    } else if starts("storage.type") {
        let primitive = ["storage.type.primitive", "storage.type.void", "storage.type.buildin"]
            .iter()
            .any(|prefix| scope.starts_with(prefix));
        if primitive || PRIMITIVE_TYPES.contains(&word) || word.starts_with(char::is_uppercase) {
            Some("type")
        } else {
            Some("keyword")
        }
    } else if starts("support.function")
        || starts("support.macro")
        || starts("support.type")
        || starts("support.class")
        || starts("support.constant")
        || starts("support.variable")
    {
        Some("built_in")
    } else if starts("entity.name.tag") {
        Some("name")
    } else if starts("entity.name")
        || starts("entity.other.inherited-class")
        || (lang != "bash" && starts("variable.function"))
    {
        Some("title")
    } else if starts("variable.parameter.option") {
        None
    } else if starts("entity.other.attribute-name") {
        Some("attr")
    } else if starts("variable.parameter") {
        Some("params")
    } else if starts("variable.language")
        || starts("variable.object.property")
        || (lang == "bash" && starts("variable.other.readwrite"))
    {
        Some("variable")
    } else if starts("punctuation.definition.tag") || (lang == "rust" && word == "->") {
        Some("punctuation")
    } else if is_meta(scope) {
        Some("meta")
    } else {
        None
    }
}
/// senpi `buildCliHighlightTheme`: the theme colour of each highlight.js class (`emphasis`,
/// `strong` and `link` are text attributes there and never produced by `scope_class`).
fn class_color(class: &str) -> Option<ThemeColor> {
    Some(match class {
        "keyword" | "name" => ThemeColor::SyntaxKeyword,
        "built_in" | "class" | "type" => ThemeColor::SyntaxType,
        "literal" | "number" => ThemeColor::SyntaxNumber,
        "regexp" | "string" => ThemeColor::SyntaxString,
        "comment" | "doctag" => ThemeColor::SyntaxComment,
        "meta" => ThemeColor::Muted,
        "function" | "title" => ThemeColor::SyntaxFunction,
        "tag" | "punctuation" => ThemeColor::SyntaxPunctuation,
        "attr" | "variable" | "params" => ThemeColor::SyntaxVariable,
        "operator" => ThemeColor::SyntaxOperator,
        "addition" => ThemeColor::ToolDiffAdded,
        "deletion" => ThemeColor::ToolDiffRemoved,
        _ => return None,
    })
}
/// Like highlight.js, the innermost scope with a class wins; inside a `meta` span only strings,
/// comments, literals and keywords keep their own class.
/// `scopes` is the stack innermost-first, stringified once per stack change; `in_meta` is whether
/// any of them is a `meta` span.
fn token_class(scopes: &[String], in_meta: bool, text: &str, lang: &str) -> Option<&'static str> {
    if lang == "json" && scopes.iter().any(|scope| scope.starts_with("meta.mapping.key")) {
        return Some("attr");
    }
    // highlight.js draws a regex literal (flags and quantifiers included) as one `regexp` span.
    if scopes.iter().any(|scope| scope.starts_with("string.regexp")) {
        return Some("regexp");
    }
    let class = scopes.iter().find_map(|scope| scope_class(scope, text, lang));
    let keeps_class = matches!(
        class.and_then(class_color),
        Some(
            ThemeColor::SyntaxString
                | ThemeColor::SyntaxComment
                | ThemeColor::SyntaxNumber
                | ThemeColor::SyntaxKeyword
        )
    );
    if !keeps_class && in_meta {
        return Some("meta");
    }
    class
}
/// One highlighted run: the highlight.js class it carries (`None` for unclassed text) and its text.
pub type HighlightRun = (Option<&'static str>, String);
/// The tokenizer behind senpi `highlight` (`utils/syntax-highlight.ts`): `code` split into runs of
/// highlight.js classes. `Err` mirrors `hljs.highlight` throwing for a language it does not know.
pub fn highlight_runs(code: &str, lang: &str) -> Result<Vec<HighlightRun>, String> {
    let all_loaded = ALL_LANGUAGES_LOADED.load(Ordering::Relaxed);
    let (language, syntax) = resolve_language(lang, all_loaded)
        .ok_or_else(|| format!("Unknown language: \"{lang}\""))?;
    tokenize(code, &language, syntax).ok_or_else(|| format!("Could not highlight {language}"))
}
fn tokenize(
    code: &str,
    language: &str,
    syntax: &syntect::parsing::SyntaxReference,
) -> Option<Vec<HighlightRun>> {
    let syntax_set = syntax_set();
    let mut parser = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut runs: Vec<HighlightRun> = Vec::new();
    let mut scopes: Vec<String> = Vec::new();
    let mut in_meta = false;
    let mut scopes_stale = false;
    let refresh = |stack: &ScopeStack, scopes: &mut Vec<String>, in_meta: &mut bool, stale: &mut bool| {
        if *stale {
            *scopes = stack.as_slice().iter().rev().map(|s| s.to_string()).collect();
            *in_meta = scopes.iter().any(|scope| is_meta(scope));
            *stale = false;
        }
    };
    for line in LinesWithEndings::from(code) {
        let ops = parser.parse_line(line, syntax_set).ok()?;
        let mut previous = 0;
        for (index, op) in ops {
            if index > previous {
                let text = &line[previous..index];
                refresh(&stack, &mut scopes, &mut in_meta, &mut scopes_stale);
                append_run(&mut runs, token_class(&scopes, in_meta, text, language), text);
            }
            stack.apply(&op).ok()?;
            scopes_stale = true;
            previous = index;
        }
        if previous < line.len() {
            let text = &line[previous..];
            refresh(&stack, &mut scopes, &mut in_meta, &mut scopes_stale);
            append_run(&mut runs, token_class(&scopes, in_meta, text, language), text);
        }
    }
    Some(runs)
}
pub fn highlight_code(theme: &Theme, code: &str, lang: Option<&str>) -> Vec<String> {
    highlight_code_with(theme, code, lang, ALL_LANGUAGES_LOADED.load(Ordering::Relaxed))
}
fn highlight_code_with(
    theme: &Theme,
    code: &str,
    lang: Option<&str>,
    all_loaded: bool,
) -> Vec<String> {
    let Some((language, syntax)) = lang.and_then(|lang| resolve_language(lang, all_loaded)) else {
        return code
            .split('\n')
            .map(|line| theme.fg(ThemeColor::MdCodeBlock, line))
            .collect();
    };
    let Some(class_runs) = tokenize(code, &language, syntax) else {
        return code.split('\n').map(str::to_owned).collect();
    };
    let mut runs: Vec<(Option<ThemeColor>, String)> = Vec::new();
    for (class, text) in &class_runs {
        append_run(&mut runs, class.and_then(class_color), text);
    }
    let mut output = String::new();
    for (color, text) in runs {
        if let Some(color) = color {
            let trimmed = text.trim_end_matches('\n');
            output.push_str(&theme.fg(color, trimmed));
            output.push_str(&text[trimmed.len()..]);
        } else {
            output.push_str(&text);
        }
    }
    // highlight.js's Rust grammar marks the binding after `let` as a variable; the TextMate
    // grammar has no scope for it.
    static LET_BINDING: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| regex::Regex::new(r"let(?:\x1b\[[0-9;]*m)*\s+([a-zA-Z_][a-zA-Z0-9_]*)").ok());
    if language == "rust"
        && let Some(re) = LET_BINDING.as_ref()
    {
        output = re
            .replace_all(&output, |caps: &regex::Captures<'_>| {
                let all = caps.get(0).map_or("", |m| m.as_str());
                let variable = caps.get(1).map_or("", |m| m.as_str());
                format!(
                    "{}{}",
                    &all[..all.len() - variable.len()],
                    theme.fg(ThemeColor::SyntaxVariable, variable)
                )
            })
            .into_owned();
    }
    output.split('\n').map(str::to_owned).collect()
}
fn append_run<T: PartialEq>(runs: &mut Vec<(Option<T>, String)>, class: Option<T>, text: &str) {
    if let Some((old, run)) = runs.last_mut()
        && *old == class
    {
        run.push_str(text);
        return;
    }
    runs.push((class, text.into()));
}
pub fn get_language_from_path(path: &str) -> Option<&'static str> {
    let extension = path.rsplit('.').next()?.to_ascii_lowercase();
    match extension.as_str() {
        "ts" => Some("typescript"),
        "tsx" => Some("typescript"),
        "js" => Some("javascript"),
        "jsx" => Some("javascript"),
        "mjs" => Some("javascript"),
        "cjs" => Some("javascript"),
        "py" => Some("python"),
        "rb" => Some("ruby"),
        "rs" => Some("rust"),
        "go" => Some("go"),
        "java" => Some("java"),
        "kt" => Some("kotlin"),
        "swift" => Some("swift"),
        "c" => Some("c"),
        "h" => Some("c"),
        "cpp" => Some("cpp"),
        "cc" => Some("cpp"),
        "cxx" => Some("cpp"),
        "hpp" => Some("cpp"),
        "cs" => Some("csharp"),
        "php" => Some("php"),
        "sh" => Some("bash"),
        "bash" => Some("bash"),
        "zsh" => Some("bash"),
        "fish" => Some("fish"),
        "ps1" => Some("powershell"),
        "sql" => Some("sql"),
        "html" => Some("html"),
        "htm" => Some("html"),
        "css" => Some("css"),
        "scss" => Some("scss"),
        "sass" => Some("sass"),
        "less" => Some("less"),
        "json" => Some("json"),
        "yaml" => Some("yaml"),
        "yml" => Some("yaml"),
        "toml" => Some("toml"),
        "xml" => Some("xml"),
        "md" => Some("markdown"),
        "markdown" => Some("markdown"),
        "dockerfile" => Some("dockerfile"),
        "makefile" => Some("makefile"),
        "cmake" => Some("cmake"),
        "lua" => Some("lua"),
        "perl" => Some("perl"),
        "r" => Some("r"),
        "scala" => Some("scala"),
        "clj" => Some("clojure"),
        "ex" => Some("elixir"),
        "exs" => Some("elixir"),
        "erl" => Some("erlang"),
        "hs" => Some("haskell"),
        "ml" => Some("ocaml"),
        "vim" => Some("vim"),
        "graphql" => Some("graphql"),
        "proto" => Some("protobuf"),
        "tf" => Some("hcl"),
        "hcl" => Some("hcl"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    //! Port of the theme-side cases of senpi `test/syntax-highlight.test.ts` (`highlightCode` and
    //! `supportsLanguage`); the `renderHighlightedHtml`/`highlight` cases live with
    //! `maho_cli::utils::syntax_highlight`. The all-languages flag is passed explicitly so the
    //! tests never flip the process-wide switch under parallel tests.
    use super::*;
    use crate::theme::ColorMode;

    const EAGER_LANGUAGES: &[&str] = &[
        "python", "java", "go", "javascript", "cpp", "typescript", "php", "ruby", "c", "csharp",
        "nix", "bash", "rust", "scala", "kotlin", "swift", "dart", "groovy", "perl", "lua",
    ];

    fn theme() -> Theme {
        Theme::builtin("dark", ColorMode::Truecolor).expect("builtin dark theme")
    }

    #[test]
    fn loads_the_twenty_most_common_languages_at_startup_and_defers_the_rest() {
        assert!(
            EAGER_LANGUAGES
                .iter()
                .all(|lang| resolve_language(lang, false).is_some())
        );
        assert!(resolve_language("ada", false).is_none());
        assert!(resolve_language("ada", true).is_some());
    }

    #[test]
    fn colors_diff_additions_and_deletions_in_fenced_diff_blocks() {
        let theme = theme();
        let lines = highlight_code_with(&theme, "-old\n+new\n", Some("diff"), true);

        let removed = theme.get_fg_ansi(ThemeColor::ToolDiffRemoved);
        let added = theme.get_fg_ansi(ThemeColor::ToolDiffAdded);
        assert_eq!(lines[0], format!("{removed}-old\x1b[39m"));
        assert_eq!(lines[1], format!("{added}+new\x1b[39m"));
    }

    #[test]
    fn keeps_cli_highlight_default_styled_scopes_mapped_to_theme_styles() {
        let theme = theme();
        let javascript =
            highlight_code_with(&theme, "const re = /foo+/gi;", Some("javascript"), true);
        let string = theme.get_fg_ansi(ThemeColor::SyntaxString);
        assert!(
            javascript[0].contains(&format!("{string}/foo+/gi\x1b[39m")),
            "{javascript:?}"
        );
        assert_eq!(
            highlight_code_with(&theme, "@decorator", Some("python"), true)[0],
            format!("{}@decorator\x1b[39m", theme.get_fg_ansi(ThemeColor::Muted))
        );
        let html = highlight_code_with(&theme, "<div></div>", Some("html"), true);
        let keyword = theme.get_fg_ansi(ThemeColor::SyntaxKeyword);
        assert!(html[0].contains(&format!("{keyword}div\x1b[39m")), "{html:?}");
    }
}
