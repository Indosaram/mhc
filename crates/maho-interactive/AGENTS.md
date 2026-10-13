# crates/maho-interactive

## OVERVIEW
Interactive terminal session orchestrator, UI presentation components, and streaming display handlers.

## WHERE TO LOOK
| Task | Location | Notes |
|------|----------|-------|
| Terminal components | `src/components/` | Prompt, status indicators, markdown, diff viewers |
| Streaming reveal & pacing | `src/streaming_reveal.rs` | Token animation and smooth rendering loop integration |
| Streaming pacing config | `src/streaming_reveal_pacing.rs` | Speed governors and rate calculation |
| Content buffering | `src/streaming_reveal_content.rs` | Chunk ingestion and layout preservation |
| Provider error rendering | `src/provider_error_presentation.rs` | Formatted user-facing error notices |
| Session format & tips | `src/session_info_format.rs`, `src/tips/` | Session banners and contextual workflow tips |
| Theming & styles | `src/theme/` | ANSI styling, color palettes, and syntax themes |

## CONVENTIONS
- Component rendering builds upon senpi string/ANSI differential renderer.
- Preserve deterministic layout bounds to prevent terminal flicker during redraws.
- Clean up alternate screen buffers on interrupt or exit signals.
- Workspace lints enforced: zero unwrap across all UI routines.

## ANTI-PATTERNS (THIS COMPONENT)
- Do not introduce ratatui dependencies or ratatui buffer conventions.
- Never write raw unbuffered ANSI escapes outside the managed renderer pipeline.
- Avoid variable timing delays in rendering unit tests.

## NOTES
- Scored 15 due to high file count, complex ANSI rendering state, and direct UX centrality.
- All visual tests use tools/golden fixtures without ratatui.
