//! Port of omo-senpi `components/browser-bridge` at latest `455dee623c9b2f2d2f1e9e68bf7b72a878ee3f0b`.
//!
//! One upstream source file per Rust module, names and order preserved:
//! `index.ts` -> [`index`].

pub mod index;

pub use index::{
    BROWSER_BRIDGE_TOOL_NAME, BROWSER_STATE_EVENT, BROWSER_STOP_RPC,
    BrowserBridgeComponent, create_browser_bridge_component,
};
