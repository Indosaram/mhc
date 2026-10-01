use super::super::{compose::compose_fold_result, types::*};
use super::{
    grammar_assets::{GrammarResolver, resolve_grammar},
    syntax::{SyntaxNode, TreeSitterLanguage, fold_ranges_from_syntax},
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};
pub const TREE_SITTER_PARSE_BUDGET_MS: u64 = 250;
pub const TREE_SITTER_FOLDER_ID: &str = "tree-sitter-wasm";
pub const TREE_SITTER_FOLDER_VERSION: &str = "1";
pub struct TreeSitterFolderOptions {
    pub fallback: Arc<dyn ReadFolder>,
    pub parse_budget_ms: u64,
    pub resolve_grammar: Option<GrammarResolver>,
    pub cache: bool,
}
impl TreeSitterFolderOptions {
    pub fn new(fallback: Arc<dyn ReadFolder>) -> Self {
        Self {
            fallback,
            parse_budget_ms: TREE_SITTER_PARSE_BUDGET_MS,
            resolve_grammar: None,
            cache: true,
        }
    }
}
type Loaded = Arc<Mutex<tree_sitter::Parser>>;
static GRAMMARS: OnceLock<Mutex<HashMap<TreeSitterLanguage, Option<Loaded>>>> = OnceLock::new();
fn grammars() -> &'static Mutex<HashMap<TreeSitterLanguage, Option<Loaded>>> {
    GRAMMARS.get_or_init(Mutex::default)
}
fn load_parser(language: TreeSitterLanguage, options: &TreeSitterFolderOptions) -> Option<Loaded> {
    let grammar = options
        .resolve_grammar
        .as_ref()
        .map_or_else(|| resolve_grammar(language), |resolve| resolve(language))?;
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&grammar).ok()?;
    Some(Arc::new(Mutex::new(parser)))
}
struct TreeSitterFolder {
    parser: Loaded,
    budget: Duration,
    fallback: Arc<dyn ReadFolder>,
}
impl ReadFolder for TreeSitterFolder {
    fn id(&self) -> &str {
        TREE_SITTER_FOLDER_ID
    }
    fn version(&self) -> &str {
        TREE_SITTER_FOLDER_VERSION
    }
    fn fold(&self, input: ReadFolderInput<'_>) -> ReadFolderResult {
        let Ok(mut parser) = self.parser.lock() else {
            return self.fallback.fold(input);
        };
        let started = Instant::now();
        let mut progress = |_: &tree_sitter::ParseState| started.elapsed() > self.budget;
        let bytes = input.text.as_bytes();
        let tree = parser.parse_with_options(
            &mut |offset, _| bytes.get(offset..).unwrap_or(&[]),
            None,
            Some(tree_sitter::ParseOptions::new().progress_callback(&mut progress)),
        );
        parser.reset();
        drop(parser);
        let Some(tree) = tree else {
            return self.fallback.fold(input);
        };
        let scan = fold_ranges_from_syntax(
            &SyntaxNode::from_node(tree.root_node(), input.text),
            input.settings,
        );
        if matches!(scan, ReadBraceScan::ParseFailure { .. }) {
            self.fallback.fold(input)
        } else {
            compose_fold_result(input.text, scan)
        }
    }
}
pub fn load_tree_sitter_folder(
    language: TreeSitterLanguage,
    options: TreeSitterFolderOptions,
) -> Option<Arc<dyn ReadFolder>> {
    let parser = if options.cache {
        let mut cache = grammars().lock().ok()?;
        cache
            .entry(language)
            .or_insert_with(|| load_parser(language, &options))
            .clone()?
    } else {
        load_parser(language, &options)?
    };
    Some(Arc::new(TreeSitterFolder {
        parser,
        budget: Duration::from_millis(options.parse_budget_ms),
        fallback: options.fallback,
    }))
}
pub fn reset_tree_sitter_grammars() {
    match grammars().lock() {
        Ok(mut cache) => cache.clear(),
        Err(poisoned) => poisoned.into_inner().clear(),
    }
}
