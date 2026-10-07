//! Recall message renderer (latest `recall/render.ts`).
//!
//! Builds the late-hidden nudge block injected as a hint message: one sourced block per judged
//! nudge, consumed by the harness-side recall wiring.

use super::gate::RecallNudge;

/// English header naming the sender and the reference-only posture.
pub const RECALL_HINT_HEADER: &str = "Kibitzer, a background memory advisor, surfaced this stored note. It may or may not apply: reference only; your current task stands.";

/// Korean header for a hint written in Korean.
pub const RECALL_HINT_HEADER_KO: &str = "백그라운드 메모리 조언자 키비처가 짚어준 저장 메모입니다. 맞을 수도 아닐 수도 있으니 참고만 하고, 하던 작업은 그대로 이어가세요.";

/// Renders one judged nudge as a sourced, escaped `<recalled-memory>` block.
pub fn render_nudge_block(nudge: &RecallNudge) -> String {
    let header = if nudge.hint.chars().any(|ch| ('\u{AC00}'..='\u{D7A3}').contains(&ch)) {
        RECALL_HINT_HEADER_KO
    } else {
        RECALL_HINT_HEADER
    };
    format!(
        "<recalled-memory source=\"[[{}]]\">\n{}\n{}\n</recalled-memory>",
        escape_markup(&nudge.path),
        header,
        escape_markup(&nudge.hint),
    )
}

fn escape_markup(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
