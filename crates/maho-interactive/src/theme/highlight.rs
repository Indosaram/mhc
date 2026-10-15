use super::theme::{Theme, ThemeColor};
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
/// Maps one TextMate scope onto the highlight.js class senpi's `buildCliHighlightTheme` would
/// colour, returning that class's theme colour. `None` lets the enclosing scope decide.
fn scope_color(scope: &str, text: &str, lang: &str) -> Option<ThemeColor> {
    let word = text.trim();
    let is_word = word.chars().any(char::is_alphanumeric);
    let starts = |prefix: &str| scope.starts_with(prefix);
    if starts("comment") {
        Some(ThemeColor::SyntaxComment)
    } else if starts("string") || starts("storage.type.string") {
        Some(ThemeColor::SyntaxString)
    } else if starts("constant.numeric") || starts("constant.language") || starts("storage.type.numeric") {
        Some(ThemeColor::SyntaxNumber)
    } else if starts("keyword.operator") {
        // Word operators (`as`, `new`, `typeof`) are keywords in highlight.js; symbolic operators
        // carry the `operator` class only in the grammars that emit it.
        if is_word {
            Some(ThemeColor::SyntaxKeyword)
        } else if matches!(lang, "swift" | "nix") {
            Some(ThemeColor::SyntaxOperator)
        } else {
            None
        }
    } else if starts("keyword") || starts("storage.modifier") {
        Some(ThemeColor::SyntaxKeyword)
    } else if !is_word && starts("storage.type") {
        None
    } else if starts("storage.type") {
        let primitive = ["storage.type.primitive", "storage.type.void", "storage.type.buildin"]
            .iter()
            .any(|prefix| scope.starts_with(prefix));
        if primitive || PRIMITIVE_TYPES.contains(&word) || word.starts_with(char::is_uppercase) {
            Some(ThemeColor::SyntaxType)
        } else {
            Some(ThemeColor::SyntaxKeyword)
        }
    } else if starts("support.function")
        || starts("support.macro")
        || starts("support.type")
        || starts("support.class")
        || starts("support.constant")
        || starts("support.variable")
    {
        Some(ThemeColor::SyntaxType)
    } else if starts("entity.name.tag") {
        Some(ThemeColor::SyntaxKeyword)
    } else if starts("entity.name")
        || starts("entity.other.inherited-class")
        || (lang != "bash" && starts("variable.function"))
    {
        Some(ThemeColor::SyntaxFunction)
    } else if starts("variable.parameter.option") {
        None
    } else if starts("entity.other.attribute-name")
        || starts("variable.parameter")
        || starts("variable.language")
        || starts("variable.object.property")
        || (lang == "bash" && starts("variable.other.readwrite"))
    {
        Some(ThemeColor::SyntaxVariable)
    } else if starts("punctuation.definition.tag") || (lang == "rust" && word == "->") {
        Some(ThemeColor::SyntaxPunctuation)
    } else if is_meta(scope) {
        Some(ThemeColor::Muted)
    } else {
        None
    }
}
/// Like highlight.js, the innermost scope with a colour wins; inside a `meta` span only strings,
/// comments, literals and keywords keep their own colour.
/// `scopes` is the stack innermost-first, stringified once per stack change; `in_meta` is whether
/// any of them is a `meta` span.
fn token_color(scopes: &[String], in_meta: bool, text: &str, lang: &str) -> Option<ThemeColor> {
    let color = scopes.iter().find_map(|scope| scope_color(scope, text, lang));
    let keeps_color = matches!(
        color,
        Some(
            ThemeColor::SyntaxString
                | ThemeColor::SyntaxComment
                | ThemeColor::SyntaxNumber
                | ThemeColor::SyntaxKeyword
        )
    );
    if !keeps_color && in_meta {
        return Some(ThemeColor::Muted);
    }
    color
}
pub fn highlight_code(theme: &Theme, code: &str, lang: Option<&str>) -> Vec<String> {
    let syntax_set = syntax_set();
    let language = lang.and_then(eager_language);
    let syntax = language.and_then(|(_, name)| syntax_set.find_syntax_by_name(name));
    let (Some((language, _)), Some(syntax)) = (language, syntax) else {
        return code
            .split('\n')
            .map(|line| theme.fg(ThemeColor::MdCodeBlock, line))
            .collect();
    };
    let mut parser = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut output = String::new();
    let mut runs: Vec<(Option<ThemeColor>, String)> = Vec::new();
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
        let Ok(ops) = parser.parse_line(line, syntax_set) else {
            return code.split('\n').map(str::to_owned).collect();
        };
        let mut previous = 0;
        for (index, op) in ops {
            if index > previous {
                let text = &line[previous..index];
                refresh(&stack, &mut scopes, &mut in_meta, &mut scopes_stale);
                append_run(&mut runs, token_color(&scopes, in_meta, text, language), text);
            }
            if stack.apply(&op).is_err() {
                return code.split('\n').map(str::to_owned).collect();
            }
            scopes_stale = true;
            previous = index;
        }
        if previous < line.len() {
            let text = &line[previous..];
            refresh(&stack, &mut scopes, &mut in_meta, &mut scopes_stale);
            append_run(&mut runs, token_color(&scopes, in_meta, text, language), text);
        }
    }
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
fn append_run(runs: &mut Vec<(Option<ThemeColor>, String)>, color: Option<ThemeColor>, text: &str) {
    if let Some((old, run)) = runs.last_mut()
        && *old == color
    {
        run.push_str(text);
        return;
    }
    runs.push((color, text.into()));
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
