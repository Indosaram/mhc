//! Port of omo-senpi `components/builtin-mcps` at latest `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! One upstream source file per Rust module, names preserved: `index.ts` -> [`index`].

pub mod index;

pub use index::{
    BUILTIN_MCPS_COMPONENT_NAME, BuiltinMcpsComponent, BuiltinMcpsComponentOptions,
    CONTEXT7_API_KEY_ENV, CONTEXT7_SERVER_NAME, CONTEXT7_URL, DEFERRED_EXPOSURE, GREP_APP_SERVER_NAME,
    GREP_APP_URL, builtin_mcp_declarations, create_context7_declaration, grep_app_declaration,
    has_context7_api_key,
};
