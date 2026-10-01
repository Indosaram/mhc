use maho_core::compaction::{compaction::CompactionPreparation, warm_anchor::{create_warm_anchor_snapshot, is_warm_summary_anchor_valid}};
use maho_ext_api::{ApplyCompactionOptions, ApplyCompactionResult, CompactionReason, CompactionResult, ExtensionContext, ExtensionFailure};
use serde_json::{Value, json};
use crate::prompts::PromptVariant;

#[derive(Clone, Debug)]
pub struct SpeculativeCompactionSnapshot {
    pub generation: u64,
    pub expected_revision: u64,
    pub model: maho_ai::types::Model,
    pub context_window: u64,
    pub preparation: CompactionPreparation,
    pub branch_entries: Vec<Value>,
    pub prompt_variant: PromptVariant,
    pub custom_instructions: Option<String>,
    pub system_prompt: Option<String>,
    pub tools: Vec<maho_ai::types::Tool>,
}

pub fn get_prompt_variant(reason: &str, preparation: &CompactionPreparation) -> PromptVariant {
    if reason == "branch" { PromptVariant::Branch }
    else if preparation.previous_summary.as_ref().is_some_and(|summary| !summary.is_empty()) { PromptVariant::Update }
    else if preparation.is_split_turn { PromptVariant::TurnPrefix }
    else { PromptVariant::Default }
}

pub fn branch_values(context: &ExtensionContext) -> Vec<Value> {
    context.session_manager.get_branch().into_iter().map(|entry| {
        let mut data = entry.data;
        data["id"] = json!(entry.id);
        data["parentId"] = json!(entry.parent_id);
        data["timestamp"] = json!(entry.timestamp);
        data["type"] = json!(entry.kind);
        data
    }).collect()
}

pub fn create_speculative_compaction_snapshot(
    context: &ExtensionContext,
    generation: u64,
    custom_instructions: Option<String>,
    tools: Vec<maho_ai::types::Tool>,
) -> Result<Option<SpeculativeCompactionSnapshot>, ExtensionFailure> {
    let Some(model) = context.model.clone() else { return Ok(None); };
    let expected_revision = context.get_message_revision()?;
    let branch_entries = branch_values(context);
    let context_window = context.get_context_usage()?.map_or(model.context_window, |usage| usage.context_window);
    let live_settings = context.get_compaction_settings()?;
    let mut settings = maho_core::compaction::settings::default_compaction_settings();
    settings.enabled = live_settings.enabled;
    settings.reserve_tokens = i64::try_from(live_settings.reserve_tokens).expect("reserve tokens fit native compaction settings");
    settings.keep_recent_tokens = crate::policy::compute_effective_keep_recent_tokens(
        live_settings.keep_recent_tokens as f64, context_window as f64,
        crate::policy::compute_effective_threshold(context_window as f64, None), 0.05,
    ) as i64;
    let Some(preparation) = maho_core::compaction::compaction::prepare_compaction(&branch_entries, &settings, false, false) else { return Ok(None); };
    let prompt_variant = get_prompt_variant("extension", &preparation);
    Ok(Some(SpeculativeCompactionSnapshot { generation, expected_revision, model,
        context_window, preparation, branch_entries, prompt_variant, custom_instructions,
        system_prompt: Some(context.get_system_prompt()), tools }))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeculativeCompactionResult { Applied, Stale, Rejected, Unavailable, Failed }

pub async fn apply_generated_compaction(
    context: &ExtensionContext,
    snapshot: Option<&SpeculativeCompactionSnapshot>,
    current_generation: u64,
    compaction: Option<CompactionResult>,
    signal: Option<maho_ext_api::AbortSignal>,
) -> Result<SpeculativeCompactionResult, ExtensionFailure> {
    if context.model.as_ref().is_some_and(|model| matches!(model.provider.as_str(), "cursor" | "cursor-cli-oauth")) && !context.is_idle() {
        return Ok(SpeculativeCompactionResult::Rejected);
    }
    let (Some(snapshot), Some(compaction)) = (snapshot, compaction) else { return Ok(SpeculativeCompactionResult::Unavailable); };
    if snapshot.generation != current_generation { return Ok(SpeculativeCompactionResult::Stale); }
    let revision_unchanged = snapshot.expected_revision == context.get_message_revision()?;
    let warm_anchor = create_warm_anchor_snapshot(&snapshot.preparation.first_kept_entry_id, &snapshot.branch_entries);
    if !revision_unchanged && !warm_anchor.as_ref().is_some_and(|anchor| is_warm_summary_anchor_valid(anchor, &branch_values(context))) {
        return Ok(SpeculativeCompactionResult::Stale);
    }
    let options = ApplyCompactionOptions {
        reason: CompactionReason::Extension,
        expected_revision: revision_unchanged.then_some(snapshot.expected_revision),
        expected_warm_anchor: if revision_unchanged { None } else { warm_anchor.map(|anchor| maho_ext_api::WarmAnchorSnapshot {
            first_kept_entry_id: anchor.first_kept_entry_id,
            prefix_entry_ids: anchor.prefix_entry_ids,
            latest_compaction_entry_id: anchor.latest_compaction_entry_id,
        }) },
        signal,
    };
    Ok(match context.apply_compaction(compaction, options).await? {
        ApplyCompactionResult::Applied => SpeculativeCompactionResult::Applied,
        ApplyCompactionResult::Stale => SpeculativeCompactionResult::Stale,
        ApplyCompactionResult::Rejected => SpeculativeCompactionResult::Rejected,
    })
}
