//! Single integration-test binary for this crate: each file under tests/ is one module, so
//! the crate links one test executable instead of one per file.

#[path = "components32_boundary.rs"]
mod components32_boundary;
#[path = "components32_callonly.rs"]
mod components32_callonly;
#[path = "components32_cards.rs"]
mod components32_cards;
#[path = "components32_custom.rs"]
mod components32_custom;
#[path = "components32_descriptors.rs"]
mod components32_descriptors;
#[path = "components32_extra.rs"]
mod components32_extra;
#[path = "components32_fallback.rs"]
mod components32_fallback;
#[path = "components32_foundation.rs"]
mod components32_foundation;
#[path = "components32_mouse.rs"]
mod components32_mouse;
#[path = "components32_overflow.rs"]
mod components32_overflow;
#[path = "components32_renderers.rs"]
mod components32_renderers;
#[path = "components32_support.rs"]
mod components32_support;
#[path = "components32_tools.rs"]
mod components32_tools;
#[path = "components32_transcript.rs"]
mod components32_transcript;
#[path = "extension_ui_repaint.rs"]
mod extension_ui_repaint;
#[path = "footer_corpus.rs"]
mod footer_corpus;
#[path = "foundation.rs"]
mod foundation;
#[path = "grok_mermaid_edges.rs"]
mod grok_mermaid_edges;
#[path = "keybinding_hints_global.rs"]
mod keybinding_hints_global;
#[path = "mermaid_golden.rs"]
mod mermaid_golden;
#[path = "presentation.rs"]
mod presentation;
#[path = "runtime35_external_editor.rs"]
mod runtime35_external_editor;
#[path = "runtime35_favorites.rs"]
mod runtime35_favorites;
#[path = "runtime35_helpers.rs"]
mod runtime35_helpers;
#[path = "runtime35_native.rs"]
mod runtime35_native;
#[path = "runtime35_render.rs"]
mod runtime35_render;
#[path = "runtime35_renderers.rs"]
mod runtime35_renderers;
#[path = "runtime35_replay.rs"]
mod runtime35_replay;
#[path = "runtime35_transfer.rs"]
mod runtime35_transfer;
#[path = "runtime35_viewport.rs"]
mod runtime35_viewport;
#[path = "runtime35_widget_clock.rs"]
mod runtime35_widget_clock;
#[path = "task33_search.rs"]
mod task33_search;
#[path = "task33_selectors.rs"]
mod task33_selectors;
#[path = "task34.rs"]
mod task34;
#[path = "tui_render_root.rs"]
mod tui_render_root;
#[path = "upstream_regressions.rs"]
mod upstream_regressions;
