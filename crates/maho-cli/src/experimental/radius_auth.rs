use crate::cli::experimental::command_options::AuthInput;
pub const ENV_RADIUS_GATEWAY: &str = "PI_RADIUS_GATEWAY";
pub async fn explicit_token(input: Option<&AuthInput>, cwd: &str, signal: Option<&maho_ai::utils::abort::AbortSignal>) -> Result<Option<String>, String> {
    if signal.is_some_and(maho_ai::utils::abort::AbortSignal::aborted) { return Err("The operation was aborted".to_owned()); }
    let Some(input) = input else { return Ok(None); };
    let value = match input {
        AuthInput::Token(token) => token.clone(),
        AuthInput::File(path) => {
            let path = crate::utils::paths::resolve_path(path, cwd, &Default::default())?;
            let read = tokio::fs::read_to_string(path);
            if let Some(signal) = signal { tokio::select! { biased; _ = signal.cancelled() => return Err("The operation was aborted".to_owned()), value = read => value.map_err(|error| error.to_string())? } }
            else { read.await.map_err(|error| error.to_string())? }
        }
    };
    let token = value.trim();
    if token.is_empty() { return Err("Radius authentication token must not be empty".to_owned()); }
    Ok(Some(token.to_owned()))
}
