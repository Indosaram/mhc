use std::future::Future;
use super::{model_miss::{classify_retryable_model_miss, ModelMissResult, RetryableModelMiss}, run_artifacts::RunAttempt};

#[derive(Clone, Debug)]
pub struct ReflectionModelCandidate { pub model: String, pub thinking: Option<String> }
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
        let mut models = Vec::new(); let mut providers = Vec::new();
        for attempt in &self.attempts { match &attempt.miss { RetryableModelMiss::ModelNotVisible { id } => models.push(id.as_str()), RetryableModelMiss::AuthMissing { provider } => providers.push(provider.as_str()) } }
        let mut parts = Vec::new();
        if !models.is_empty() { parts.push(format!("model_not_visible:{}", models.join(","))); }
        if !providers.is_empty() { parts.push(format!("auth_missing:{}", providers.join(","))); }
        parts.push(format!("attempted:{}", self.attempts.iter().map(|attempt| attempt.candidate.model.as_str()).collect::<Vec<_>>().join(",")));
        f.write_str(&parts.join("; "))
    }
}
impl std::error::Error for MemoryModelExhaustedError {}
pub async fn run_memory_model_attempts<F, Fut>(first: ReflectionModelCandidate, rest: &[ReflectionModelCandidate], mut attempt: F) -> Result<MemoryModelAttempt, MemoryModelExhaustedError>
where F: FnMut(ReflectionModelCandidate, usize, Option<RunAttempt>) -> Fut, Fut: Future<Output = ReflectionChildResult> {
    let candidates: Vec<_> = std::iter::once(&first).chain(rest.iter()).collect();
    let mut misses = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        let next_attempt = candidates.get(index + 1).map(|next| RunAttempt { attempt: u32::try_from(index + 2).unwrap_or(u32::MAX), model: next.model.clone(), thinking: next.thinking.clone() });
        let child = attempt((*candidate).clone(), index + 1, next_attempt).await;
        let miss = classify_retryable_model_miss(&ModelMissResult { code: child.code, stdout: &child.stdout, stderr: &child.stderr, timed_out: child.timed_out });
        let Some(miss) = miss else { return Ok(MemoryModelAttempt { candidate: (*candidate).clone(), child }); };
        misses.push(ExhaustedMemoryModelAttempt { candidate: (*candidate).clone(), miss });
    }
    Err(MemoryModelExhaustedError { attempts: misses })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn candidates() -> (ReflectionModelCandidate, Vec<ReflectionModelCandidate>) { (ReflectionModelCandidate { model: "extension-only/primary".into(), thinking: Some("off".into()) }, vec![ReflectionModelCandidate { model: "builtin/fallback".into(), thinking: Some("minimal".into()) }]) }
    fn child(code: i32, stderr: &str, timed_out: bool) -> ReflectionChildResult { ReflectionChildResult { code: Some(code), signal: None, stdout: String::new(), stderr: stderr.into(), timed_out } }
    #[tokio::test]
    async fn missing_model_retries_fallback() { let (first, rest) = candidates(); let mut attempted = Vec::new(); let result = run_memory_model_attempts(first, &rest, |candidate, number, next| { attempted.push(candidate.model); if number == 1 { assert_eq!(next.unwrap().attempt, 2); } std::future::ready(if number == 1 { child(1, "Error: Model \"extension-only/primary\" not found. Use --list-models to see available models.", false) } else { child(0, "", false) }) }).await.unwrap(); assert_eq!(attempted, ["extension-only/primary", "builtin/fallback"]); assert_eq!(result.child.code, Some(0)); }
    #[tokio::test]
    async fn missing_auth_retries_fallback() { let (first, rest) = candidates(); let result = run_memory_model_attempts(first, &rest, |_, number, _| std::future::ready(if number == 1 { child(1, "No API key found for extension-only", false) } else { child(0, "", false) })).await.unwrap(); assert_eq!(result.candidate.model, "builtin/fallback"); }
    #[tokio::test]
    async fn exhausted_chain_preserves_causes() { let (first, rest) = candidates(); let error = run_memory_model_attempts(first, &rest, |_, number, _| std::future::ready(if number == 1 { child(1, "Error: Model \"extension-only/primary\" not found. Use --list-models to see available models.", false) } else { child(1, "No API key found for anthropic", false) })).await.unwrap_err(); assert_eq!(error.attempts.len(), 2); assert_eq!(error.to_string(), "model_not_visible:extension-only/primary; auth_missing:anthropic; attempted:extension-only/primary,builtin/fallback"); }
    #[tokio::test]
    async fn generic_failure_not_retried() { let (first, rest) = candidates(); let mut count = 0; let result = run_memory_model_attempts(first, &rest, |_, _, _| { count += 1; std::future::ready(child(1, "provider request failed", false)) }).await.unwrap(); assert_eq!(count, 1); assert_eq!(result.child.stderr, "provider request failed"); }
    #[tokio::test]
    async fn timeout_not_retried() { let (first, rest) = candidates(); let mut count = 0; let result = run_memory_model_attempts(first, &rest, |_, _, _| { count += 1; std::future::ready(child(1, "", true)) }).await.unwrap(); assert_eq!(count, 1); assert!(result.child.timed_out); }
}
