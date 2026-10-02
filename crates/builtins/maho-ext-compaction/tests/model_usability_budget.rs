use maho_ai::types::{Model,Tool};
use maho_core::compaction::settings::default_compaction_settings;
use maho_ext_compaction::model_usability_budget::*;
use serde_json::json;
fn model(provider:&str,id:&str,window:u64,max:u64)->Model {serde_json::from_value(json!({"id":id,"name":id,"api":"faux-completion","provider":provider,"baseUrl":"","reasoning":false,"input":["text"],"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"contextWindow":window,"maxTokens":max})).expect("model")}
fn tools()->Vec<Tool> {serde_json::from_value(json!([{"name":"read","description":"Read a file from the workspace.","parameters":{"type":"object","properties":{"path":{"type":"string"}},"required":["path"]}},{"name":"bash","description":"Run a shell command in the workspace.","parameters":{"type":"object","properties":{"command":{"type":"string"},"timeout":{"type":"number"}},"required":["command"]}}])).expect("tools")}
fn project(provider:&str,id:&str,window:u64,max:u64,live:f64)->ModelUsabilityBudgetProjection {project_model_usability_budget(ModelUsabilityBudgetInput {model:&model(provider,id,window,max),system_prompt:"You are a coding agent operating on a fixed session budget.",tools:&tools(),live_context_tokens:Some(live),compaction:&default_compaction_settings(),include_speculation_lead:None,admission:None})}
#[test] fn comfortable_model_matches_fixture_budget() {let p=project("faux","faux-comfortable",200000,32000,25000.);assert_eq!(p.system_prompt_tokens,15.);assert_eq!(p.active_tool_schema_tokens,88.);assert_eq!(p.required_tokens,99179.);assert!(p.usable);}
#[test] fn small_switch_has_exact_shortfall() {let p=project("faux","faux-shortfall",48000,8000,25000.);assert_eq!(p.required_tokens,65871.);assert_eq!(p.shortfall_tokens,17871.);assert!(!p.usable);}
#[test] fn empty_session_removes_live_context_shortfall() {let p=project("faux","faux-shortfall",48000,8000,0.);assert_eq!(p.required_tokens,40871.);assert!(p.usable);}
#[test] fn anthropic_family_has_larger_safety_margin() {let a=project("anthropic","claude-sonnet",200000,32000,25000.);let b=project("faux","default",200000,32000,25000.);assert_eq!(a.safety_margin_profile,"anthropic");assert_eq!(a.required_tokens-b.required_tokens,8192.);}
#[test] fn family_markers_respect_boundaries() {for (id,profile) in [("proxy/gpt-5.1","openai-reasoning"),("notgpt-5","default"),("gpt-50","default"),("gemini-2","google"),("deepseek/chat","deepseek")] {assert_eq!(project("proxy",id,200000,32000,0.).safety_margin_profile,profile);}}
#[test] fn resume_relaxation_requires_both_reduction_geometries_to_fit() {let m=model("faux","resume",100000,30000);let settings=default_compaction_settings();let p=project_model_usability_budget(ModelUsabilityBudgetInput {model:&m,system_prompt:"",tools:&[],live_context_tokens:Some(55000.),compaction:&settings,include_speculation_lead:Some(false),admission:Some(ModelUsabilityAdmission::Resume)});assert!(p.usable);assert_eq!(p.verdict,ModelUsabilityVerdict::FitsAfterCompaction);}
#[test] fn disabled_compaction_omits_reserve_and_lead() {let m=model("faux","disabled",100000,30000);let mut settings=default_compaction_settings();settings.enabled=false;let p=project_model_usability_budget(ModelUsabilityBudgetInput {model:&m,system_prompt:"",tools:&[],live_context_tokens:None,compaction:&settings,include_speculation_lead:None,admission:None});assert_eq!(p.compaction_reserve_tokens,0.);assert_eq!(p.speculation_lead_tokens,0.);}
#[test] fn pending_switch_targets_its_own_keep_recent_geometry() {let p=project("faux","small",48000,8000,25000.);let expected=p.post_compaction_required_tokens-(p.required_tokens-p.live_context_tokens);let pending=maho_ext_compaction::switch_admission::create_pending_model_switch(model("faux","small",48000,8000),p.clone(),true);assert!(pending.persist_default);assert_eq!(maho_ext_compaction::switch_admission::pending_switch_keep_recent_tokens(&pending.projection),expected.max(1.));let requirement=maho_ext_compaction::resume_admission::create_resume_compaction_requirement(p.clone());assert_eq!(requirement.projection,p);}
#[test] fn resume_slice_fits_without_mutating_transcript() {
    let p=project("faux","small",48000,8000,25000.);
    let entries:Vec<_>=(0..20).map(|i|json!({"type":"message","id":format!("e{i}"),"parentId":if i==0 {None} else {Some(format!("e{}",i-1))},"timestamp":"2025-01-01T00:00:00.000Z","message":{"role":"user","content":"x".repeat(6000),"timestamp":i}})).collect();
    let before=entries.clone();let slice=maho_ext_compaction::resume_slice::plan_resume_slice(&entries,&p).expect("slice");
    let overhead=p.required_tokens-p.live_context_tokens;assert!(slice.tokens_after+overhead<=p.context_window);assert!(slice.dropped_entries>0);assert_eq!(entries,before);
}
#[test]
fn persisted_resume_slice_reopens_with_budgeted_context_and_complete_transcript() {
    use maho_core::session_manager::SessionManager;
    use maho_ext_compaction::resume_slice::*;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().to_str().unwrap();
    let mut session = SessionManager::create(path, Some(path), None);
    for index in 0..20 {
        session.append_message(json!({"role":"user","content":"x".repeat(6000),"timestamp":index}));
    }
    session.append_message(json!({"role":"assistant","content":[{"type":"text","text":"recorded"}],"api":"faux-completion","provider":"faux","model":"small","usage":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"totalTokens":0,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":20}));
    let file = session.session_file().unwrap().to_owned();
    drop(session);
    let mut reopened = SessionManager::open(&file, Some(path), None, None);
    let transcript = reopened.entries();
    let projection = project("faux", "small", 48000, 8000, 25000.);
    let plan = plan_resume_slice(&reopened.branch(None), &projection).unwrap();
    reopened.append_compaction(&plan.summary, &plan.first_kept_entry_id, plan.tokens_before as i64,
        Some(json!({"schema":RESUME_SLICE_SCHEMA,"origin":RESUME_SLICE_ORIGIN})), None, Some(false));
    drop(reopened);
    let restored = SessionManager::open(&file, Some(path), None, None);
    assert_eq!(&restored.entries()[..transcript.len()], transcript.as_slice());
    let measured: u64 = restored.build_context(None).messages.iter().map(maho_core::compaction::compaction::estimate_tokens).sum();
    assert_eq!(measured as f64, plan.tokens_after);
    assert!(measured as f64 + projection.required_tokens - projection.live_context_tokens <= projection.context_window);
    assert_eq!(restored.entries().last().unwrap()["firstKeptEntryId"], plan.first_kept_entry_id);
}
