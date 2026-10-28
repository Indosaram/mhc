use std::future::Future;
use super::{model_miss::{classify_retryable_model_miss, ModelMissResult, RetryableModelMiss}, run_artifacts::RunAttempt};

pub const MODEL_UNREACHABLE_MARKER: &str = "model_unreachable_with_extensions";
pub fn is_model_unreachable_detail(detail: Option<&str>) -> bool {
    detail.is_some_and(|detail| detail.starts_with(MODEL_UNREACHABLE_MARKER))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflectionModelCandidate { pub model: String, pub thinking: Option<String>, pub load_extensions: Option<bool> }
#[derive(Debug)]
pub struct ReflectionChildResult { pub code: Option<i32>, pub signal: Option<String>, pub stdout: String, pub stderr: String, pub timed_out: bool }
#[derive(Debug)]
pub struct MemoryModelAttempt { pub candidate: ReflectionModelCandidate, pub child: ReflectionChildResult }
#[derive(Debug)]
pub struct ExhaustedMemoryModelAttempt { pub candidate: ReflectionModelCandidate, pub miss: RetryableModelMiss }
#[derive(Debug)]
pub struct MemoryModelExhaustedError { pub attempts: Vec<ExhaustedMemoryModelAttempt> }
impl std::fmt::Display for MemoryModelExhaustedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut models = Vec::new(); let mut providers = Vec::new(); let mut overflows = Vec::new(); let mut outages = Vec::new();
        for attempt in &self.attempts { match &attempt.miss { RetryableModelMiss::ModelNotVisible { id } => models.push(id.as_str()), RetryableModelMiss::AuthMissing { provider } => providers.push(provider.as_str()), RetryableModelMiss::ContextOverflow { detail } => overflows.push(detail.as_str()), RetryableModelMiss::ProviderUnavailable { detail } => outages.push(detail.as_str()) } }
        let mut parts = Vec::new();
        if self.attempts.iter().all(|attempt| matches!(attempt.miss, RetryableModelMiss::ModelNotVisible { .. }) && attempt.candidate.load_extensions == Some(true)) {
            parts.push(MODEL_UNREACHABLE_MARKER.to_owned());
        }
        if !models.is_empty() { parts.push(format!("model_not_visible:{}", models.join(","))); }
        if !overflows.is_empty() { parts.push(format!("context_overflow:{}", overflows.join(" | "))); }
        if !providers.is_empty() { parts.push(format!("auth_missing:{}", providers.join(","))); }
        if !outages.is_empty() { parts.push(format!("provider_unavailable:{}", outages.join(" | "))); }
        parts.push(format!("attempted:{}", self.attempts.iter().map(|attempt| attempt.candidate.model.as_str()).collect::<Vec<_>>().join(",")));
        f.write_str(&parts.join("; "))
    }
}
impl std::error::Error for MemoryModelExhaustedError {}
#[derive(Debug)]
pub enum MemoryModelAttemptError { Exhausted(MemoryModelExhaustedError), Launch(String) }
impl std::fmt::Display for MemoryModelAttemptError {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result {match self{Self::Exhausted(error)=>error.fmt(f),Self::Launch(error)=>f.write_str(error)}}
}
impl std::error::Error for MemoryModelAttemptError {}
pub async fn run_memory_model_attempts<F, Fut>(first: ReflectionModelCandidate, rest: &[ReflectionModelCandidate], mut attempt: F) -> Result<MemoryModelAttempt, MemoryModelAttemptError>
where F: FnMut(ReflectionModelCandidate, usize, Option<RunAttempt>) -> Fut, Fut: Future<Output = Result<ReflectionChildResult,String>> {
    let candidates: Vec<_> = std::iter::once(&first).chain(rest.iter()).collect();
    let mut misses = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let next_attempt = candidates.get(index + 1).map(|next| RunAttempt { attempt: u32::try_from(index + 2).unwrap_or(u32::MAX), model: next.model.clone(), thinking: next.thinking.clone() });
        let child = attempt((*candidate).clone(), index + 1, next_attempt).await.map_err(MemoryModelAttemptError::Launch)?;
        let miss = classify_retryable_model_miss(&ModelMissResult { code: child.code, stdout: &child.stdout, stderr: &child.stderr, timed_out: child.timed_out });
        let Some(miss) = miss else { return Ok(MemoryModelAttempt { candidate: (*candidate).clone(), child }); };
        misses.push(ExhaustedMemoryModelAttempt { candidate: (*candidate).clone(), miss });
    }
    Err(MemoryModelAttemptError::Exhausted(MemoryModelExhaustedError { attempts: misses }))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn extension_enabled_visibility_exhaustion_leads_with_unreachable_marker() {
        let (mut first,mut rest)=candidates();
        first.load_extensions=Some(true);
        rest[0].load_extensions=Some(true);
        let error=run_memory_model_attempts(first,&rest,|candidate,_,_|std::future::ready(Ok(child(1,&format!("Error: Model \"{}\" not found. Use --list-models to see available models.",candidate.model),false)))).await.unwrap_err();
        assert!(is_model_unreachable_detail(Some(&error.to_string())));
        let MemoryModelAttemptError::Exhausted(error)=error else{panic!("expected exhaustion")};
        assert_eq!(error.attempts.len(),2);
        assert!(error.attempts.iter().all(|attempt|attempt.candidate.load_extensions==Some(true)));
    }
    #[tokio::test]
    async fn mixed_extension_modes_do_not_claim_models_unreachable() {
        let (mut first,rest)=candidates();
        first.load_extensions=Some(true);
        let error=run_memory_model_attempts(first,&rest,|candidate,_,_|std::future::ready(Ok(child(1,&format!("Error: Model \"{}\" not found. Use --list-models to see available models.",candidate.model),false)))).await.unwrap_err();
        assert!(!is_model_unreachable_detail(Some(&error.to_string())));
    }
    fn candidates() -> (ReflectionModelCandidate, Vec<ReflectionModelCandidate>) { (ReflectionModelCandidate { model: "extension-only/primary".into(), thinking: Some("off".into()) , load_extensions: None }, vec![ReflectionModelCandidate { model: "builtin/fallback".into(), thinking: Some("minimal".into()) , load_extensions: None }]) }
    fn child(code: i32, stderr: &str, timed_out: bool) -> ReflectionChildResult { ReflectionChildResult { code: Some(code), signal: None, stdout: String::new(), stderr: stderr.into(), timed_out } }
    #[tokio::test]
    async fn missing_model_retries_fallback() { let (first, rest) = candidates(); let mut attempted = Vec::new(); let result = run_memory_model_attempts(first, &rest, |candidate, number, next| { attempted.push(candidate.model); if number == 1 { assert_eq!(next.unwrap().attempt, 2); } std::future::ready(Ok(if number == 1 { child(1, "Error: Model \"extension-only/primary\" not found. Use --list-models to see available models.", false) } else { child(0, "", false) })) }).await.unwrap(); assert_eq!(attempted, ["extension-only/primary", "builtin/fallback"]); assert_eq!(result.child.code, Some(0)); }
    #[tokio::test]
    async fn missing_auth_retries_fallback() { let (first, rest) = candidates(); let result = run_memory_model_attempts(first, &rest, |_, number, _| std::future::ready(Ok(if number == 1 { child(1, "No API key found for extension-only", false) } else { child(0, "", false) }))).await.unwrap(); assert_eq!(result.candidate.model, "builtin/fallback"); }
    #[tokio::test]
    async fn exhausted_chain_preserves_causes() { let (first, rest) = candidates(); let error = run_memory_model_attempts(first, &rest, |_, number, _| std::future::ready(Ok(if number == 1 { child(1, "Error: Model \"extension-only/primary\" not found. Use --list-models to see available models.", false) } else { child(1, "No API key found for anthropic", false) }))).await.unwrap_err(); assert_eq!(error.to_string(), "model_not_visible:extension-only/primary; auth_missing:anthropic; attempted:extension-only/primary,builtin/fallback"); let MemoryModelAttemptError::Exhausted(error)=error else{panic!("expected exhaustion")};assert_eq!(error.attempts.len(), 2); }
    #[tokio::test]
    async fn generic_failure_not_retried() { let (first, rest) = candidates(); let mut count = 0; let result = run_memory_model_attempts(first, &rest, |_, _, _| { count += 1; std::future::ready(Ok(child(1, "provider request failed", false))) }).await.unwrap(); assert_eq!(count, 1); assert_eq!(result.child.stderr, "provider request failed"); }
    #[tokio::test]
    async fn timeout_not_retried() { let (first, rest) = candidates(); let mut count = 0; let result = run_memory_model_attempts(first, &rest, |_, _, _| { count += 1; std::future::ready(Ok(child(1, "", true))) }).await.unwrap(); assert_eq!(count, 1); assert!(result.child.timed_out); }
    #[tokio::test]
    async fn launch_error_propagates_without_retry_or_fake_child() {let (first,rest)=candidates();let mut count=0;let error=run_memory_model_attempts(first,&rest,|_,_,_|{count+=1;std::future::ready(Err("spawn failed".into()))}).await.unwrap_err();assert_eq!(count,1);assert!(matches!(error,MemoryModelAttemptError::Launch(ref error) if error=="spawn failed"));}
    #[tokio::test]
    async fn context_overflow_retries_fallback() {
        let (first, rest) = candidates();
        let mut attempted = Vec::new();
        let result = run_memory_model_attempts(first, &rest, |candidate, _, _| {
            attempted.push(candidate.model.clone());
            std::future::ready(Ok(if candidate.model == "extension-only/primary" {
                child(1, "Your input exceeds the context window of this model", false)
            } else {
                child(0, "", false)
            }))
        })
        .await
        .unwrap();
        assert_eq!(attempted, ["extension-only/primary", "builtin/fallback"]);
        assert_eq!(result.candidate.model, "builtin/fallback");
    }

    #[tokio::test]
    async fn provider_cooldown_503_retries_fallback() {
        let (first, rest) = candidates();
        let mut attempted = Vec::new();
        let result = run_memory_model_attempts(first, &rest, |candidate, _, _| {
            attempted.push(candidate.model.clone());
            std::future::ready(Ok(if candidate.model == "extension-only/primary" {
                child(1, "503: {\"message\":\"All providers are temporarily cooling down\"}", false)
            } else {
                child(0, "", false)
            }))
        })
        .await
        .unwrap();
        assert_eq!(attempted, ["extension-only/primary", "builtin/fallback"]);
        assert_eq!(result.candidate.model, "builtin/fallback");
        assert_eq!(result.child.code, Some(0));
    }

    #[tokio::test]
    async fn quota_exhaustion_retries_fallback() {
        let (first, rest) = candidates();
        let mut attempted = Vec::new();
        let result = run_memory_model_attempts(first, &rest, |candidate, _, _| {
            attempted.push(candidate.model.clone());
            std::future::ready(Ok(if candidate.model == "extension-only/primary" {
                child(1, "Error: quota exceeded for this organization", false)
            } else {
                child(0, "", false)
            }))
        })
        .await
        .unwrap();
        assert_eq!(attempted, ["extension-only/primary", "builtin/fallback"]);
        assert_eq!(result.candidate.model, "builtin/fallback");
        assert_eq!(result.child.code, Some(0));
    }

    #[tokio::test]
    async fn every_candidate_overflows_exhaustion() {
        let first = ReflectionModelCandidate {
            model: "openai/primary".into(),
            thinking: None,
         load_extensions: None };
        let rest = vec![ReflectionModelCandidate {
            model: "anthropic/fallback".into(),
            thinking: None,
         load_extensions: None }];
        let error = run_memory_model_attempts(first, &rest, |_, _, _| {
            std::future::ready(Ok(child(
                1,
                "prompt is too long: 213462 tokens > 200000 maximum",
                false,
            )))
        })
        .await
        .unwrap_err();
        let MemoryModelAttemptError::Exhausted(error) = error else {
            panic!("expected exhaustion");
        };
        assert!(error.to_string().contains("context_overflow:"));
        assert_eq!(error.attempts.len(), 2);
        assert!(matches!(
            &error.attempts[0].miss,
            RetryableModelMiss::ContextOverflow { .. }
        ));
        assert!(matches!(
            &error.attempts[1].miss,
            RetryableModelMiss::ContextOverflow { .. }
        ));
    }

}
