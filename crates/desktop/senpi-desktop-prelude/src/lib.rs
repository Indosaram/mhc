//! Native Rust port of `@oh-my-opencode/senpi-desktop-prelude`.
//!
//! Provides the `computer` eval-kernel contribution, prelude assets,
//! and model documentation for the senpi computer tool.

pub mod assets;

pub use assets::{prelude_assets, PreludeAssets, ASSETS_JSON_RAW};

/// Every method a `computer` call chain may name: the union of the protocol's desktop, window, and element tier
/// tables. The facades emit nothing else.
pub const METHOD_ALLOWLIST: &[&str] = &[
    "actions",
    "attributes",
    "ax",
    "bounds",
    "capabilities",
    "children",
    "click",
    "clipboard.read",
    "clipboard.write",
    "displays",
    "doubleClick",
    "drag",
    "elementAt",
    "find",
    "focus",
    "focusedElement",
    "focusedWindow",
    "move",
    "parent",
    "perform",
    "press",
    "raise",
    "ref",
    "screenshot",
    "scroll",
    "setValue",
    "type",
    "value",
    "window",
    "windows",
];

pub const COMPUTER_EXPORTS: &[&str] = &["computer"];

/// The `computer` eval-kernel contribution plus its model docs.
pub struct ComputerPreludeAssets;

impl ComputerPreludeAssets {
    pub fn javascript() -> &'static str {
        &prelude_assets().computer_prelude_javascript
    }

    pub fn python() -> &'static str {
        &prelude_assets().computer_prelude_python
    }

    /// TypeScript declarations of the JavaScript `computer` global.
    pub fn declarations() -> &'static str {
        &prelude_assets().computer_declarations
    }

    /// Helper-list lines for the eval prompt's prelude block.
    pub fn documentation() -> &'static str {
        &prelude_assets().computer_documentation
    }

    /// System-prompt fragment for sessions where the `computer` tool is active.
    pub fn safety() -> &'static str {
        &prelude_assets().computer_safety
    }

    pub fn exports() -> &'static [&'static str] {
        COMPUTER_EXPORTS
    }

    pub fn method_allowlist() -> &'static [&'static str] {
        METHOD_ALLOWLIST
    }
}
