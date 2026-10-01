use super::engine::*;
use crate::definition::AbortSignal;
pub async fn search(request: GrepEngineRequest, signal: &AbortSignal) -> Result<GrepEngineResult, GrepEngineError> {
    if signal.is_aborted() { return Err(GrepEngineError::Aborted); }
    if request.pcre2 == Some(true) { return Err(GrepEngineError::UnsupportedRegex("Native grep does not support PCRE2".into())); }
    let cancel = maho_grep::CancelToken::new(request.timeout_ms);
    let aborted = std::sync::Arc::clone(&cancel.aborted);
    let mut operation = tokio::task::spawn_blocking(move || maho_grep::search(&request, &cancel));
    tokio::select! {
        result = &mut operation => result.map_err(|e| GrepEngineError::EngineUnavailable(e.to_string()))?,
        () = signal.cancelled() => {
            aborted.store(true, std::sync::atomic::Ordering::Release);
            match operation.await.map_err(|e| GrepEngineError::EngineUnavailable(e.to_string()))? {
                Ok(_) | Err(GrepEngineError::Aborted) => {},
                Err(error) => return Err(error),
            }
            Err(GrepEngineError::Aborted)
        }
    }
}
