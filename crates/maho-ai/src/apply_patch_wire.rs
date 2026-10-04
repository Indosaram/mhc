//! Port of `getApplyPatchWireMode` from senpi `core/extensions/builtin/gpt-apply-patch/extension.ts`.
//! It lives in `maho-ai` because `maho-ext-gpt-apply-patch` (renderer -> `maho-interactive`) and
//! `maho-ext-ask-user` (depended on by `maho-interactive`) both need it; keeping it in the
//! gpt-apply-patch crate would form a crate cycle. `maho-ext-gpt-apply-patch` re-exports both items
//! unchanged, so its senpi-mapped public paths stay identical.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

static GPT_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)(?:^|[/@:._-])gpt(?:[._-]|[0-9])").expect("literal pattern"));

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ApplyPatchWireMode { Freeform, Json, None }

pub fn get_apply_patch_wire_mode(model: Option<(&str, &str)>) -> ApplyPatchWireMode {
    let Some((api, id)) = model else { return ApplyPatchWireMode::None; };
    if !GPT_ID.is_match(id) { return ApplyPatchWireMode::None; }
    match api { "openai-responses" | "azure-openai-responses" | "openai-codex-responses" => ApplyPatchWireMode::Freeform, "openai-completions" => ApplyPatchWireMode::Json, _ => ApplyPatchWireMode::None }
}

pub fn is_openai_gpt_model(model: Option<(&str, &str)>) -> bool { get_apply_patch_wire_mode(model) == ApplyPatchWireMode::Freeform }

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpt_ids_on_responses_apis_take_freeform() {
        for api in ["openai-responses", "azure-openai-responses", "openai-codex-responses"] {
            assert_eq!(get_apply_patch_wire_mode(Some((api, "gpt-6-astra"))), ApplyPatchWireMode::Freeform);
        }
        assert_eq!(get_apply_patch_wire_mode(Some(("openai-completions", "gpt-5.6-sol"))), ApplyPatchWireMode::Json);
    }

    #[test]
    fn non_gpt_and_unknown_apis_take_none() {
        assert_eq!(get_apply_patch_wire_mode(None), ApplyPatchWireMode::None);
        assert_eq!(get_apply_patch_wire_mode(Some(("openai-responses", "deepseek-v3-gptq"))), ApplyPatchWireMode::None);
        assert_eq!(get_apply_patch_wire_mode(Some(("anthropic-messages", "gpt-6-astra"))), ApplyPatchWireMode::None);
        assert!(!is_openai_gpt_model(Some(("openai-completions", "gpt-5.6-sol"))));
        assert!(is_openai_gpt_model(Some(("openai-responses", "codex/gpt-6-astra"))));
    }
}
