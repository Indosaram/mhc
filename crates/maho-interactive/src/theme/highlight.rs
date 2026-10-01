use super::theme::{Theme, ThemeColor};
use syntect::{
    parsing::{ParseState, ScopeStack, SyntaxSet},
    util::LinesWithEndings,
};
fn token_color(scope: &str) -> Option<ThemeColor> {
    if scope.contains("comment") {
        Some(ThemeColor::SyntaxComment)
    } else if scope.contains("string") {
        Some(ThemeColor::SyntaxString)
    } else if scope.contains("constant.numeric") || scope.contains("constant.language") {
        Some(ThemeColor::SyntaxNumber)
    } else if scope.contains("entity.name.function") {
        Some(ThemeColor::SyntaxFunction)
    } else if scope.contains("variable.parameter") {
        Some(ThemeColor::SyntaxVariable)
    } else if scope.contains("storage.type")
        || scope.contains("storage.modifier")
        || scope.contains("keyword") && !scope.contains("operator")
    {
        Some(ThemeColor::SyntaxKeyword)
    } else if scope.contains("support.type") || scope.contains("entity.name.class") {
        Some(ThemeColor::SyntaxType)
    } else {
        None
    }
}
pub fn highlight_code(theme: &Theme, code: &str, lang: Option<&str>) -> Vec<String> {
    let syntax_set = SyntaxSet::load_defaults_newlines();
    let syntax = lang
        .filter(|l| !matches!(*l, "json" | "yaml" | "toml"))
        .and_then(|lang| syntax_set.find_syntax_by_token(lang));
    let Some(syntax) = syntax else {
        return code
            .split('\n')
            .map(|line| theme.fg(ThemeColor::MdCodeBlock, line))
            .collect();
    };
    let mut parser = ParseState::new(syntax);
    let mut stack = ScopeStack::new();
    let mut output = String::new();
    let mut runs: Vec<(Option<ThemeColor>, String)> = Vec::new();
    for line in LinesWithEndings::from(code) {
        let Ok(ops) = parser.parse_line(line, &syntax_set) else {
            return code.split('\n').map(str::to_owned).collect();
        };
        let mut previous = 0;
        for (index, op) in ops {
            if index > previous {
                let scopes = stack
                    .as_slice()
                    .iter()
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
                    .join(" ");
                let color = token_color(&scopes);
                append_run(&mut runs, color, &line[previous..index]);
            }
            if stack.apply(&op).is_err() {
                return code.split('\n').map(str::to_owned).collect();
            }
            previous = index;
        }
        if previous < line.len() {
            let scopes = stack
                .as_slice()
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>()
                .join(" ");
            append_run(&mut runs, token_color(&scopes), &line[previous..]);
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
    if lang == Some("rust")
        && let Ok(re) = regex::Regex::new(r"let(?:\x1b\[[0-9;]*m)*\s+([a-zA-Z_][a-zA-Z0-9_]*)")
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
