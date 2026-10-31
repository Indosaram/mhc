//! Port of senpi `packages/coding-agent/src/core/browser-engine.ts`.
//!
//! The typed enum lives in `maho_tool_child::context::BrowserEngine` (the tool child reads it from the
//! wire); this module carries the string vocabulary and the guard the core uses.

pub use maho_tool_child::context::BrowserEngine;

pub const BROWSER_ENGINES: [&str; 3] = ["connected", "builtin", "none"];

/// The variable a session's tool subprocesses and eval kernels read the engine from.
pub const BROWSER_ENGINE_ENV: &str = "OMO_BROWSER_ENGINE";

pub fn is_browser_engine(value: &str) -> bool {
    BROWSER_ENGINES.contains(&value)
}

/// The engine a wire value names, when it names one.
pub fn parse_browser_engine(value: &str) -> Option<BrowserEngine> {
    match value {
        "connected" => Some(BrowserEngine::Connected),
        "builtin" => Some(BrowserEngine::Builtin),
        "none" => Some(BrowserEngine::None),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_only_the_three_engines() {
        for engine in BROWSER_ENGINES {
            assert!(is_browser_engine(engine));
            assert!(parse_browser_engine(engine).is_some());
        }
        assert!(!is_browser_engine("chrome"));
        assert!(!is_browser_engine(""));
        assert_eq!(parse_browser_engine("Builtin"), None);
        assert_eq!(BROWSER_ENGINE_ENV, "OMO_BROWSER_ENGINE");
    }
}
