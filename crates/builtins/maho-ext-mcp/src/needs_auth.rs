use crate::{auth::oauth::{OAuthRequestError},errors::{McpError,McpErrorKind}};
pub fn is_mcp_needs_auth_error(error:&McpError)->bool {
    if error.kind==McpErrorKind::Auth{return true;}
    let mut cause=error.cause.as_deref();
    for _ in 0..5 {
        let Some(value)=cause else{return false;};
        if value.get("kind").and_then(serde_json::Value::as_str)==Some("auth") || value.get("terminal")==Some(&serde_json::Value::Bool(true)){return true;}
        cause=value.get("cause");
    }
    false
}
pub fn is_oauth_needs_auth_error(error:&OAuthRequestError)->bool {
    matches!(error,OAuthRequestError::Flow(error) if error.terminal)
}
