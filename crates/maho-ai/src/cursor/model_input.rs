//! Port of senpi packages/ai/src/cursor/model-input.ts.
//!
//! Model-id families Cursor accepts images for. `GetUsableModels` carries no modality field, so
//! this follows AvailableModels' `supportsImages` flag (live capture 2026-10-18, senpi#3620).

const MULTIMODAL_FRAGMENTS: &[&str] = &[
    "claude",
    "gemini",
    "gpt-",
    "codex",
    "kimi-k3",
    "muse-",
    "cursor-grok-4.5",
    "cursor-grok-4.6",
];

pub fn cursor_discovered_input(model_id: &str) -> Vec<String> {
    let id = model_id.to_lowercase();
    let multimodal = id == "default"
        || MULTIMODAL_FRAGMENTS
            .iter()
            .any(|fragment| id.contains(fragment));
    if multimodal {
        vec!["text".to_owned(), "image".to_owned()]
    } else {
        vec!["text".to_owned()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_families_take_images() {
        for id in [
            "default",
            "kimi-k3",
            "muse-spark-1.3",
            "cursor-grok-4.6",
            "claude-opus-5-5",
        ] {
            assert_eq!(cursor_discovered_input(id), ["text", "image"], "{id}");
        }
        for id in [
            "composer-2.5",
            "kimi-k2.7-code",
            "grok-4.7",
            "grok-code-fast-1",
        ] {
            assert_eq!(cursor_discovered_input(id), ["text"], "{id}");
        }
    }
}
