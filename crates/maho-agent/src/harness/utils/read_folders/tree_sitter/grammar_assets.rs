use super::syntax::TreeSitterLanguage;
pub const TREE_SITTER_RUNTIME_FILE: &str = "web-tree-sitter.wasm";
pub const GRAMMAR_FILES: [(TreeSitterLanguage, &str); 3] = [
    (TreeSitterLanguage::Ts, "typescript.wasm"),
    (TreeSitterLanguage::Tsx, "tsx.wasm"),
    (TreeSitterLanguage::Js, "javascript.wasm"),
];
pub type GrammarResolver =
    std::sync::Arc<dyn Fn(TreeSitterLanguage) -> Option<tree_sitter::Language> + Send + Sync>;
pub fn resolve_grammar(language: TreeSitterLanguage) -> Option<tree_sitter::Language> {
    Some(match language {
        TreeSitterLanguage::Js => tree_sitter_javascript::LANGUAGE.into(),
        TreeSitterLanguage::Ts => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        TreeSitterLanguage::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
    })
}
