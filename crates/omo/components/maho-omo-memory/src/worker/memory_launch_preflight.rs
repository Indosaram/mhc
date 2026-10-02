use std::{collections::BTreeMap, future::Future};
use super::{memory_model_attempts::{MemoryModelAttempt, ReflectionChildResult, ReflectionModelCandidate, run_memory_model_attempts}, model_preflight::{ConfigSource, Launcher, ModelPreflight, PreflightResult, PREFLIGHT_TIMEOUT_MS}, run_artifacts::RunAttempt};

pub struct MemoryLaunchPreflightInput<'a> {
    pub first: ReflectionModelCandidate,
    pub rest: &'a [ReflectionModelCandidate],
    pub launch: &'a Launcher,
    pub env: &'a BTreeMap<String, String>,
    pub env_flag: &'a str,
    pub config_sources: &'a [ConfigSource],
    pub surface_name: &'a str,
    pub now_ms: i64,
}

pub async fn resolve_and_preflight_memory_launch<F, Fut>(cache: &mut ModelPreflight, input: MemoryLaunchPreflightInput<'_>, warn: impl FnOnce(&str), attempt: F) -> Result<MemoryModelAttempt, String>
where F: FnMut(ReflectionModelCandidate, usize, Option<RunAttempt>) -> Fut, Fut: Future<Output = ReflectionChildResult> {
    let candidates: Vec<_> = std::iter::once(input.first).chain(input.rest.iter().cloned()).collect();
    let mut env = input.env.clone();
    env.insert(input.env_flag.into(), "1".into());
    env.insert("SENPI_PTY_FORCE_PIPE".into(), "1".into());
    let (preflight, warning) = cache.preflight(&candidates, input.launch, &env, input.config_sources, input.now_ms, PREFLIGHT_TIMEOUT_MS).await;
    if let Some(warning) = warning { warn(&warning); }
    let candidates = match preflight {
        PreflightResult::Filtered { candidates, .. } | PreflightResult::Unavailable { candidates } => candidates,
        PreflightResult::NoneVisible { rejected } => return Err(format!("No {} model candidate is visible to the discovery-disabled child: {}", input.surface_name, rejected.iter().map(|model| format!("{model} (model_not_visible)")).collect::<Vec<_>>().join(", "))),
    };
    let mut candidates = candidates.into_iter();
    let first = candidates.next().ok_or_else(|| "memory model chain must not be empty".to_owned())?;
    run_memory_model_attempts(first, &candidates.collect::<Vec<_>>(), attempt).await.map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn candidate(model: &str) -> ReflectionModelCandidate { ReflectionModelCandidate { model: model.into(), thinking: None } }
    fn success() -> ReflectionChildResult { ReflectionChildResult { code: Some(0), signal: None, stdout: String::new(), stderr: String::new(), timed_out: false } }
    #[tokio::test]
    async fn discovery_flags_filter_before_attempt_and_do_not_mutate_parent_env() {
        let launch = Launcher { command: "/bin/sh".into(), prefix_args: vec!["-c".into(), "[ \"$SENPI_MEMORY_FACTS\" = 1 ] && [ \"$SENPI_PTY_FORCE_PIPE\" = 1 ] && printf 'builtin/fallback\\n'".into()] };
        let env = BTreeMap::new();
        let rest = [candidate("builtin/fallback")];
        let input = MemoryLaunchPreflightInput { first: candidate("extension/primary"), rest: &rest, launch: &launch, env: &env, env_flag: "SENPI_MEMORY_FACTS", config_sources: &[], surface_name: "facts", now_ms: 0 };
        let mut attempts = vec![];
        let result = resolve_and_preflight_memory_launch(&mut ModelPreflight::default(), input, |e| panic!("{e}"), |model, number, next| { attempts.push(model.model); assert_eq!(number, 1); assert!(next.is_none()); std::future::ready(success()) }).await.unwrap();
        assert_eq!(attempts, ["builtin/fallback"]);
        assert_eq!(result.candidate.model, "builtin/fallback");
        assert!(env.is_empty());
    }
    #[tokio::test]
    async fn no_visible_model_never_launches_attempt() {
        let launch = Launcher { command: "/bin/sh".into(), prefix_args: vec!["-c".into(), "printf 'other/model\\n'".into()] };
        let env = BTreeMap::new();
        let input = MemoryLaunchPreflightInput { first: candidate("builtin/model"), rest: &[], launch: &launch, env: &env, env_flag: "SENPI_MEMORY_REFLECTION", config_sources: &[], surface_name: "reflection", now_ms: 0 };
        let mut launched = false;
        let error = resolve_and_preflight_memory_launch(&mut ModelPreflight::default(), input, |e| panic!("{e}"), |_, _, _| { launched = true; std::future::ready(success()) }).await.unwrap_err();
        assert!(!launched);
        assert!(error.contains("builtin/model (model_not_visible)"));
    }
    #[tokio::test]
    async fn unavailable_catalog_warns_and_preserves_reactive_chain() {
        let launch = Launcher { command: "/bin/sh".into(), prefix_args: vec!["-c".into(), "exit 7".into()] };
        let env = BTreeMap::new();
        let input = MemoryLaunchPreflightInput { first: candidate("builtin/model"), rest: &[], launch: &launch, env: &env, env_flag: "SENPI_MEMORY_FACTS", config_sources: &[], surface_name: "facts", now_ms: 0 };
        let mut warning = String::new();
        let result = resolve_and_preflight_memory_launch(&mut ModelPreflight::default(), input, |e| warning = e.into(), |_, _, _| std::future::ready(success())).await.unwrap();
        assert!(warning.contains('7'));
        assert_eq!(result.candidate.model, "builtin/model");
    }
}
