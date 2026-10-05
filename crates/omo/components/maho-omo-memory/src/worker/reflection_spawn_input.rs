use std::{collections::BTreeMap, path::Path};
use memory_core::{identity::resolve::MemoryIdentity, reflection::{ReflectionWorktree, ReservedRun}};
use super::{memory_model_attempts::ReflectionModelCandidate, model_preflight::Launcher, run_artifacts::RunAttempt, spawn_payload::{PrepareReflectionSpawnInput, prepare_reflection_fork_spawn, prepare_reflection_spawn}, spawn_types::{DreamPeoplePolicy, ReflectionSpawnArgs}};

pub struct ReflectionCandidateSpawnInput<'a> {
    pub parent_session_file: Option<&'a Path>, pub parent_cwd: Option<&'a Path>,
    pub run: &'a ReservedRun, pub worktree: &'a ReflectionWorktree,
    pub merge_policy: &'a str, pub category: &'a str, pub candidate: &'a ReflectionModelCandidate,
    pub attempt: u32, pub hard_deadline_at: f64, pub next_attempt: Option<RunAttempt>,
    pub config: &'a serde_json::Value, pub identity: &'a MemoryIdentity,
    pub env: BTreeMap<String, String>, pub launch: Launcher, pub now_ms: f64,
}

pub fn prepare_reflection_candidate_spawn(input: ReflectionCandidateSpawnInput<'_>) -> Result<ReflectionSpawnArgs, std::io::Error> {
    let sessions = input.identity.paths.reflection.join("runs");
    let skills = input.identity.paths.runtime.join("skills-usage.json");
    let state = input.identity.paths.runtime.join("dream/state.json");
    let base = &input.config["memory"]["people"];
    let override_policy = &input.config["memory"]["agents"][&input.identity.id]["people"];
    let field = |key: &str| override_policy.get(key).filter(|value| !value.is_null()).or_else(|| base.get(key));
    let policy = DreamPeoplePolicy { enabled: field("enabled").and_then(serde_json::Value::as_bool).unwrap_or(true), max_entries: field("max_entries").and_then(serde_json::Value::as_u64).and_then(|value| usize::try_from(value).ok()).unwrap_or(40), max_entry_chars: field("max_entry_chars").and_then(serde_json::Value::as_u64).and_then(|value| usize::try_from(value).ok()).unwrap_or(200) };
    let fork = input.parent_session_file.is_some();
    let prepared = PrepareReflectionSpawnInput { parent_session_file: input.parent_session_file, parent_cwd: input.parent_cwd, run: input.run, worktree: input.worktree, reflection_sessions_dir: &sessions, category: input.category, model: &input.candidate.model, thinking: input.candidate.thinking.as_deref(), attempt: Some(input.attempt), hard_deadline_at: Some(input.hard_deadline_at), next_attempt: input.next_attempt, env: input.env, merge_policy: input.merge_policy, skills_usage_source: &skills, dream_state_source: &state, people_policy: &policy, launch: input.launch, now_ms: input.now_ms };
    if fork { prepare_reflection_fork_spawn(prepared) } else { prepare_reflection_spawn(prepared) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn dream_uses_identity_sources_and_agent_policy_overrides() {
        let root = tempfile::tempdir().unwrap();
        let identity = MemoryIdentity { id:"agent".into(), safe_slug:"agent".into(), paths:memory_core::identity::layout::build_identity_paths(root.path(), "agent") };
        let engine = crate::engine_session::prepare_memory_engine_session("agent", &identity.paths, Default::default()).unwrap();
        let exec = memory_core::git::exec::create_git_exec(Default::default());
        let worktree = memory_core::reflection::create_reflection_worktree(&engine.repo, "run", &identity.paths.worktrees, exec.as_ref(), None).unwrap();
        let run = ReservedRun { run_id:"run".into(), request:memory_core::reflection::ReflectionRequest { trigger:memory_core::reflection::ReflectionTrigger::Dream, origin:Some(memory_core::reflection::DreamOrigin::Manual), conversation_ids:vec![], snapshots:vec![], focus:None, recent_n:None, target_doc:None }, reserved_at:None, launcher_pid:None, launcher_hostname:None, launcher_process_start:None };
        std::fs::write(identity.paths.runtime.join("skills-usage.json"), "{\"foo\":1}").unwrap();
        let candidate = ReflectionModelCandidate { model:"provider/model".into(), thinking:Some("low".into()) };
        let config = serde_json::json!({"memory":{"people":{"enabled":true,"max_entries":80,"max_entry_chars":300},"agents":{"agent":{"people":{"enabled":false,"max_entries":3}}}}});
        let result = prepare_reflection_candidate_spawn(ReflectionCandidateSpawnInput { parent_session_file:None, parent_cwd:None, run:&run, worktree:&worktree, merge_policy:"auto", category:"quick", candidate:&candidate, attempt:2, hard_deadline_at:5000.0, next_attempt:None, config:&config, identity:&identity, env:Default::default(), launch:Launcher { command:"mhc".into(), prefix_args:vec![] }, now_ms:0.0 }).unwrap();
        assert_eq!(result.attempt, 2); assert_eq!(result.hard_deadline_at, 5000.0); assert_eq!(result.model, "provider/model");
        assert_eq!(std::fs::read_to_string(result.paths.skills_usage.unwrap()).unwrap(), "{\"foo\":1}");
        let policy:serde_json::Value = super::super::run_artifacts::read_run_json(&result.paths.dream_policy.unwrap()).unwrap();
        assert_eq!(policy["people"], serde_json::json!({"enabled":false,"max_entries":3,"max_entry_chars":300}));
    }
}
