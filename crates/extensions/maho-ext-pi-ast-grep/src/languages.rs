pub const CLI_LANGUAGES: &[&str] = &["bash", "c", "cpp", "csharp", "css", "elixir", "go", "haskell", "html", "java", "javascript", "json", "kotlin", "lua", "nix", "php", "python", "ruby", "rust", "scala", "solidity", "swift", "typescript", "tsx", "yaml"];
pub const DEFAULT_TIMEOUT_MS: u64 = 300_000;
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_MATCHES: usize = 500;

pub fn extensions(language: &str) -> &'static [&'static str] {
    match language {
        "bash" => &[".bash", ".sh", ".zsh", ".bats"],
        "c" => &[".c", ".h"],
        "cpp" => &[".cpp", ".cc", ".cxx", ".hpp", ".hxx", ".h"],
        "csharp" => &[".cs"], "css" => &[".css"], "elixir" => &[".ex", ".exs"],
        "go" => &[".go"], "haskell" => &[".hs", ".lhs"], "html" => &[".html", ".htm"],
        "java" => &[".java"], "javascript" => &[".js", ".jsx", ".mjs", ".cjs"],
        "json" => &[".json"], "kotlin" => &[".kt", ".kts"], "lua" => &[".lua"], "nix" => &[".nix"],
        "php" => &[".php"], "python" => &[".py", ".pyi"], "ruby" => &[".rb", ".rake"],
        "rust" => &[".rs"], "scala" => &[".scala", ".sc"], "solidity" => &[".sol"], "swift" => &[".swift"],
        "typescript" => &[".ts", ".cts", ".mts"], "tsx" => &[".tsx"], "yaml" => &[".yml", ".yaml"],
        _ => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn language_count() { assert_eq!(CLI_LANGUAGES.len(), 25); }
    #[test] fn language_order() { assert_eq!(CLI_LANGUAGES, ["bash", "c", "cpp", "csharp", "css", "elixir", "go", "haskell", "html", "java", "javascript", "json", "kotlin", "lua", "nix", "php", "python", "ruby", "rust", "scala", "solidity", "swift", "typescript", "tsx", "yaml"]); }
    #[test] fn default_limits() { assert_eq!((DEFAULT_TIMEOUT_MS, DEFAULT_MAX_OUTPUT_BYTES, DEFAULT_MAX_MATCHES), (300_000, 1024 * 1024, 500)); }
    #[test] fn python_extensions() { assert!(extensions("python").contains(&".py")); assert!(extensions("python").contains(&".pyi")); }
}
