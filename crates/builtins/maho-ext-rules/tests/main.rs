//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "commands.rs"]
mod commands;
#[path = "config.rs"]
mod config;
#[path = "discovery.rs"]
mod discovery;
#[path = "dynamic_border.rs"]
mod dynamic_border;
#[path = "engine.rs"]
mod engine;
#[path = "factory.rs"]
mod factory;
#[path = "faux_session.rs"]
mod faux_session;
#[path = "formatter.rs"]
mod formatter;
#[path = "hooks.rs"]
mod hooks;
#[path = "matcher.rs"]
mod matcher;
#[path = "parser.rs"]
mod parser;
#[path = "rules_banner.rs"]
mod rules_banner;
#[path = "support.rs"]
mod support;
