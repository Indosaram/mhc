//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "cache.rs"]
mod cache;
#[path = "config.rs"]
mod config;
#[path = "dynamic_border.rs"]
mod dynamic_border;
#[path = "engine.rs"]
mod engine;
#[path = "finder.rs"]
mod finder;
#[path = "formatter.rs"]
mod formatter;
#[path = "index.rs"]
mod index;
#[path = "matcher.rs"]
mod matcher;
#[path = "ordering.rs"]
mod ordering;
#[path = "parser.rs"]
mod parser;
#[path = "project_root.rs"]
mod project_root;
#[path = "rules_banner.rs"]
mod rules_banner;
#[path = "scanner.rs"]
mod scanner;
#[path = "tool_paths.rs"]
mod tool_paths;
#[path = "truncator.rs"]
mod truncator;
