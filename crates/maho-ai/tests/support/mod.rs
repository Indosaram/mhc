//! Shared test support for maho-ai wire-API replay tests (todos 10-12).
//!
//! `mock_server` serves recorded fixture bytes over real HTTP, with an option to end the stream
//! early. `replay` compares a Rust `AssistantMessageEvent` sequence against the
//! golden JSON `tools/golden/ai-replay.mjs` recorded from the pinned senpi `stream()` functions for
//! the same fixture.

#![allow(dead_code)]

pub mod mock_server;
pub mod replay;
