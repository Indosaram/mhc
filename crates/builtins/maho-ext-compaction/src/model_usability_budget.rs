use maho_ai::{types::{Context, Model, Tool}, utils::estimate::estimate_context_tokens};
use maho_core::compaction::settings::CompactionSettings;
use crate::{orchestration::resolve_compaction_geometry, policy::{base_threshold_ratio_for_window, compute_effective_keep_recent_tokens}};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelUsabilityAdmission { Start, Resume, Switch }
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModelUsabilityVerdict { FitsNow, FitsAfterCompaction, Impossible }
#[derive(Clone, Debug, PartialEq)]
pub struct ModelUsabilityBudgetProjection {
    pub model: String, pub context_window: f64, pub live_context_tokens: f64,
    pub system_prompt_tokens: f64, pub active_tool_schema_tokens: f64,
    pub output_reserve_tokens: f64, pub compaction_reserve_tokens: f64,
    pub speculation_lead_tokens: f64, pub safety_margin_tokens: f64,
    pub safety_margin_profile: &'static str, pub required_tokens: f64,
    pub shortfall_tokens: f64, pub usable: bool, pub admission: ModelUsabilityAdmission,
    pub verdict: ModelUsabilityVerdict, pub post_compaction_required_tokens: f64,
    pub compaction_required_tokens: f64,
}
pub struct ModelUsabilityBudgetInput<'a> {
    pub model: &'a Model, pub system_prompt: &'a str, pub tools: &'a [Tool],
    pub live_context_tokens: Option<f64>, pub compaction: &'a CompactionSettings,
    pub include_speculation_lead: Option<bool>, pub admission: Option<ModelUsabilityAdmission>,
}
fn matches_family_marker(id: &str, marker: &str) -> bool {
    let id = id.to_lowercase();
    id.match_indices(marker).any(|(i, _)| {
        (i == 0 || id[..i].ends_with(['/', '.', ':', '_', '-']))
            && id[i + marker.len()..].chars().next().is_none_or(|c| !c.is_ascii_lowercase() && !c.is_ascii_digit())
    })
}
fn safety_margin(model: &Model) -> (&'static str, f64) {
    for (name,tokens,provider,markers) in [
        ("anthropic",16384.,"anthropic",&["claude"][..]),
        ("openai-reasoning",16384.,"openai",&["gpt-5","o1","o3","o4"][..]),
        ("google",12288.,"google",&["gemini"][..]),
        ("deepseek",12288.,"deepseek",&["deepseek"][..]),
    ] {
        if model.provider == provider || markers.iter().any(|marker| matches_family_marker(&model.id,marker)) { return (name,tokens); }
    }
    ("default",8192.)
}
pub fn project_model_usability_budget(input: ModelUsabilityBudgetInput<'_>) -> ModelUsabilityBudgetProjection {
    let window = input.model.context_window as f64;
    let live = input.live_context_tokens.unwrap_or(0.0);
    let mut context = Context {system_prompt:Some(input.system_prompt.into()),messages:Vec::new(),tools:Some(Vec::new())};
    let system = estimate_context_tokens(&context).tokens as f64;
    context.tools = Some(input.tools.to_vec());
    let tools = estimate_context_tokens(&context).tokens as f64 - system;
    let output = if input.model.max_tokens > 0 && window > 0.0 { (input.model.max_tokens as f64).min((window * 0.5).floor()) } else { 0.0 };
    let geometry = resolve_compaction_geometry(window,input.compaction,None);
    let reserve = if input.compaction.enabled {geometry.reserve_tokens} else {0.0};
    let include_lead = input.include_speculation_lead != Some(false);
    let lead = if include_lead && input.compaction.enabled && input.compaction.speculative_enabled != Some(false) {geometry.lead_tokens} else {0.0};
    let (profile,margin) = safety_margin(input.model);
    let base = system + tools + output + reserve + lead + margin;
    let compaction_required = live + system + tools + reserve + margin;
    let keep = compute_effective_keep_recent_tokens(input.compaction.keep_recent_tokens as f64,window,base_threshold_ratio_for_window(window),0.05);
    let post_required = keep + base;
    let admission = input.admission.unwrap_or(if live > 0.0 {ModelUsabilityAdmission::Switch} else {ModelUsabilityAdmission::Start});
    let mut required = live + base;
    let mut shortfall = (required - window).max(0.0);
    let verdict = if shortfall == 0.0 {ModelUsabilityVerdict::FitsNow} else if post_required <= window {ModelUsabilityVerdict::FitsAfterCompaction} else {ModelUsabilityVerdict::Impossible};
    if shortfall != 0.0 && admission == ModelUsabilityAdmission::Resume && input.compaction.enabled && !include_lead && compaction_required <= window && post_required <= window {
        required = compaction_required.max(post_required); shortfall = 0.0;
    }
    ModelUsabilityBudgetProjection { model:format!("{}/{}",input.model.provider,input.model.id),context_window:window,live_context_tokens:live,system_prompt_tokens:system,active_tool_schema_tokens:tools,output_reserve_tokens:output,compaction_reserve_tokens:reserve,speculation_lead_tokens:lead,safety_margin_tokens:margin,safety_margin_profile:profile,required_tokens:required,shortfall_tokens:shortfall,usable:shortfall==0.0,admission,verdict,post_compaction_required_tokens:post_required,compaction_required_tokens:compaction_required }
}
#[derive(Clone, Debug)]
pub struct ModelUsabilityBudgetError { pub projection: ModelUsabilityBudgetProjection }
impl std::fmt::Display for ModelUsabilityBudgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let p = &self.projection;
        let breakdown = format!("system prompt {}, active tool schemas {}, output reserve {}, compaction reserve {}, speculation lead {}, safety margin {} [{}]",p.system_prompt_tokens,p.active_tool_schema_tokens,p.output_reserve_tokens,p.compaction_reserve_tokens,p.speculation_lead_tokens,p.safety_margin_tokens,p.safety_margin_profile);
        match p.admission {
            ModelUsabilityAdmission::Resume => write!(f,"Model \"{}\" cannot resume: target context window {} tokens is {} tokens short of the {}-token requirement (live context {}, {breakdown}).",p.model,p.context_window,p.shortfall_tokens,p.required_tokens,p.live_context_tokens),
            ModelUsabilityAdmission::Switch => write!(f,"Model \"{}\" cannot switch: target context window {} tokens is {} tokens short of the {}-token requirement (live context {}, {breakdown}). Compact the session, then revalidate and retry the model switch.",p.model,p.context_window,p.shortfall_tokens,p.required_tokens,p.live_context_tokens),
            ModelUsabilityAdmission::Start => write!(f,"Model \"{}\" cannot start: context window {} tokens is {} tokens short of the {}-token minimum ({breakdown}).",p.model,p.context_window,p.shortfall_tokens,p.required_tokens),
        }
    }
}
impl std::error::Error for ModelUsabilityBudgetError {}
