//! Browser automation and computer use for mhc (MIT).
//!
//! - [`cdp`]: Chrome DevTools Protocol client over a message transport.
//! - [`transport`]: Chromium over `--remote-debugging-pipe`, over `--remote-debugging-port=0`, or
//!   an existing DevTools endpoint.
//! - [`launch`]: finding a Chromium executable and its launch switches.
//! - [`browser`] / [`page`]: a browser with named tabs; navigation, accessibility snapshots with
//!   element refs, clicks, typing, keys, scrolling, screenshots, JavaScript evaluation.
//! - [`computer`]: coordinate-level screen control ([`computer::ComputerBackend`]) over a browser
//!   viewport or a desktop/sandbox daemon.
//! - [`extension`]: the `browser` and `computer` agent tools.
//!
//! Ported from MIT sources (omowright, pi-cua-integration, oh-my-pi); see `PROVENANCE.md`.
pub mod browser;
pub mod cdp;
pub mod computer;
pub mod extension;
pub mod keys;
pub mod launch;
pub mod page;
pub mod transport;

pub use browser::{Browser, DEFAULT_TAB, LaunchOptions, TransportKind};
pub use extension::{BrowserExtension, TOOL_NAMES};
pub use page::{Page, Snapshot, SnapshotOptions, WaitUntil};
