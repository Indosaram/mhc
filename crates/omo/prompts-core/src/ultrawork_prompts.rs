use crate::types::{BundledPromptSource, PromptSource, VariantTable};

pub const ULTRAWORK_DEFAULT_PROMPT: &str = include_str!("../prompts/ultrawork/default.md");
pub const ULTRAWORK_GEMINI_PROMPT: &str = include_str!("../prompts/ultrawork/gemini.md");
pub const ULTRAWORK_GLM_PROMPT: &str = include_str!("../prompts/ultrawork/glm.md");
pub const ULTRAWORK_GPT_PROMPT: &str = include_str!("../prompts/ultrawork/gpt.md");
pub const ULTRAWORK_PLANNER_PROMPT: &str = include_str!("../prompts/ultrawork/planner.md");
pub const CODEX_ULTRAWORK_PROMPT: &str = include_str!("../prompts/ultrawork/codex.md");

#[must_use]
pub fn ultrawork_prompt_variants() -> VariantTable {
    VariantTable::from([
        ("planner", PromptSource::Bundled(BundledPromptSource {
            content: include_str!("../prompts/ultrawork/planner.md"),
            file_path: "packages/prompts-core/prompts/ultrawork/planner.md",
        })),
        ("gpt", PromptSource::Bundled(BundledPromptSource {
            content: include_str!("../prompts/ultrawork/gpt.md"),
            file_path: "packages/prompts-core/prompts/ultrawork/gpt.md",
        })),
        ("gemini", PromptSource::Bundled(BundledPromptSource {
            content: include_str!("../prompts/ultrawork/gemini.md"),
            file_path: "packages/prompts-core/prompts/ultrawork/gemini.md",
        })),
        ("glm", PromptSource::Bundled(BundledPromptSource {
            content: include_str!("../prompts/ultrawork/glm.md"),
            file_path: "packages/prompts-core/prompts/ultrawork/glm.md",
        })),
        ("default", PromptSource::Bundled(BundledPromptSource {
            content: include_str!("../prompts/ultrawork/default.md"),
            file_path: "packages/prompts-core/prompts/ultrawork/default.md",
        })),
    ])
}

#[must_use]
pub fn codex_ultrawork_prompt_variants() -> VariantTable {
    VariantTable::from([
        ("codex", PromptSource::Bundled(BundledPromptSource {
            content: include_str!("../prompts/ultrawork/codex.md"),
            file_path: "packages/prompts-core/prompts/ultrawork/codex.md",
        })),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_exports_ordered_ultrawork_and_bundled_codex() {
        assert_eq!(ultrawork_prompt_variants().keys().copied().collect::<Vec<_>>(), ["planner", "gpt", "gemini", "glm", "default"]);
        let codex = codex_ultrawork_prompt_variants();
        let PromptSource::Bundled(source) = &codex["codex"] else { panic!("Codex must be bundled"); };
        assert_eq!(source.file_path, "packages/prompts-core/prompts/ultrawork/codex.md");
        let shipped = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/ultrawork/codex.md")).expect("shipped Codex prompt exists");
        assert_eq!(source.content, shipped);
    }
}
