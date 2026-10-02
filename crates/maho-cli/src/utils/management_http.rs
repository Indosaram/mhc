use maho_ai::utils::abort::AbortSignal;
use std::time::Duration;
pub struct FetchRetryOptions {
    pub max_retries: usize, pub retry_on_status: bool,
    pub timeout_ms: Option<u64>, pub attempt_timeout_ms: Option<u64>,
}
impl Default for FetchRetryOptions {
    fn default() -> Self { Self { max_retries: 2, retry_on_status: true, timeout_ms: None, attempt_timeout_ms: None } }
}
pub async fn fetch_with_retry(client: &reqwest::Client, request: reqwest::Request, signal: Option<&AbortSignal>, options: FetchRetryOptions) -> Result<reqwest::Response, String> {
    let deadline = options.timeout_ms.filter(|value| *value > 0).map(|value| tokio::time::Instant::now() + Duration::from_millis(value));
    for attempt in 0..=options.max_retries {
        if signal.is_some_and(AbortSignal::aborted) { return Err("The operation was aborted".to_owned()); }
        if deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) { return Err("The operation timed out".to_owned()); }
        let request = request.try_clone().ok_or("Management request body must be replayable")?;
        let execute = async { client.execute(request).await.map_err(|error| error.to_string()) };
        let bounded = async {
            if let Some(timeout) = options.attempt_timeout_ms.filter(|value| *value > 0) {
                tokio::time::timeout(Duration::from_millis(timeout), execute).await.map_err(|_| "Attempt timed out".to_owned())?
            } else { execute.await }
        };
        let overall = async {
            if let Some(deadline) = deadline {
                tokio::time::timeout_at(deadline, bounded).await.map_err(|_| "The operation timed out".to_owned())?
            } else { bounded.await }
        };
        let result = if let Some(signal) = signal {
            tokio::select! { biased; _ = signal.cancelled() => return Err("The operation was aborted".to_owned()), result = overall => result }
        } else { overall.await };
        match result {
            Ok(response) => {
                if !options.retry_on_status || !matches!(response.status().as_u16(), 408 | 425 | 429 | 500 | 502 | 503 | 504) || attempt == options.max_retries { return Ok(response); }
            }
            Err(error) => {
                if signal.is_some_and(AbortSignal::aborted) || deadline.is_some_and(|deadline| tokio::time::Instant::now() >= deadline) || attempt == options.max_retries { return Err(error); }
            }
        }
    }
    unreachable!("last attempt always returns")
}
