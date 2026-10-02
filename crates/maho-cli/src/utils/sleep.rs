pub async fn sleep(ms: u64, signal: Option<&maho_ai::utils::abort::AbortSignal>) -> Result<(), String> {
    let timer = tokio::time::sleep(std::time::Duration::from_millis(ms)); if let Some(signal) = signal { if signal.aborted() { return Err("Aborted".to_owned()); } tokio::select! { () = timer => Ok(()), () = signal.cancelled() => Err("Aborted".to_owned()) } } else { timer.await; Ok(()) }
}
