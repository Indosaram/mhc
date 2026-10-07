//! Port of the pinned `packages/mcp-client-core/src/mcp-oauth/step-up.ts`.
//!
//! `parseWwwAuthenticate` / `mergeScopes` / `isStepUpRequired` are the challenge surface the
//! pinned `skill-mcp-manager/oauth-handler.ts` consumes: `handleStepUpIfNeeded` reacts to a 403
//! carrying a `WWW-Authenticate` challenge, and `manager.ts::withOperationRetry` wraps every
//! tool/resource/prompt operation in the bounded retry that feeds it.

/// The pinned `StepUpInfo`: the scopes a `WWW-Authenticate` challenge demands, plus the RFC 6750
/// `error` / `error_description` carried onto the info.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StepUpInfo {
    pub required_scopes: Vec<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Pinned `step-up.ts::parseWwwAuthenticate`: `None` unless the header names `bearer` and carries
/// a non-empty `scope` parameter (quoted or bare).
pub fn parse_www_authenticate(header: &str) -> Option<StepUpInfo> {
    let trimmed = header.trim();
    let lower = trimmed.to_ascii_lowercase();
    let bearer = lower.find("bearer")?;
    let params = trimmed[bearer + "bearer".len()..].trim();
    if params.is_empty() { return None; }
    let scope = extract_param(params, "scope")?;
    let required_scopes: Vec<String> = scope.split_whitespace().map(str::to_owned).collect();
    if required_scopes.is_empty() { return None; }
    Some(StepUpInfo { required_scopes, error: extract_param(params, "error"), error_description: extract_param(params, "error_description") })
}

/// Pinned `step-up.ts::mergeScopes`: a stable union that preserves the existing order.
pub fn merge_scopes(existing: &[String], required: &[String]) -> Vec<String> {
    let mut merged = existing.to_vec();
    for scope in required { if !merged.contains(scope) { merged.push(scope.clone()); } }
    merged
}

/// Pinned `step-up.ts::isStepUpRequired`: a step-up is required only for status `403` with a
/// parseable `WWW-Authenticate` challenge.
pub fn is_step_up_required(status_code: u16, www_authenticate: Option<&str>) -> Option<StepUpInfo> {
    if status_code != 403 { return None; }
    parse_www_authenticate(www_authenticate?)
}

/// Pinned `extractParam`: the quoted form `${name}="([^"]*)"` wins over the bare
/// `${name}=([^\s,]+)`, each taking the leftmost match.
fn extract_param(params: &str, name: &str) -> Option<String> {
    let quoted = format!("{name}=\"");
    let mut from = 0;
    while let Some(index) = params[from..].find(&quoted) {
        let start = from + index + quoted.len();
        if let Some(end) = params[start..].find('"') { return Some(params[start..start + end].to_owned()); }
        from = from + index + 1;
    }
    let bare = format!("{name}=");
    let mut from = 0;
    while let Some(index) = params[from..].find(&bare) {
        let start = from + index + bare.len();
        let end = params[start..].find(|character: char| character.is_whitespace() || character == ',').unwrap_or(params.len() - start);
        if end > 0 { return Some(params[start..start + end].to_owned()); }
        from = from + index + 1;
    }
    None
}
