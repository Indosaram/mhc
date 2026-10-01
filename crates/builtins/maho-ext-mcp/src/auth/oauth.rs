use super::oauth_errors::{OAuthFailureKind,OAuthFlowError};
pub fn parse_redirect(input:&str,server:&str)->Result<(String,Option<String>),Box<OAuthFlowError>> {
    let url=url::Url::parse(input.trim()).map_err(|_|Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {server} received a malformed redirect URL; paste the full http://127.0.0.1/... address from your browser."))))?;
    let params=url.query_pairs().collect::<Vec<_>>();
    if let Some((_,error))=params.iter().find(|(key,_)|key=="error") {return Err(Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {server} authorization failed: {error}"))));}
    let code=params.iter().find(|(key,_)|key=="code").map(|(_,value)|value.to_string()).filter(|value|!value.is_empty()).ok_or_else(||Box::new(OAuthFlowError::new(OAuthFailureKind::NeedsAuth,format!("MCP server {server} redirect URL has no authorization code; ensure you copied the entire address."))))?;
    Ok((code,params.iter().find(|(key,_)|key=="state").map(|(_,value)|value.to_string())))
}
pub fn is_rejected_authorization_code(message:&str)->bool {
    let message=message.to_lowercase();message.contains("authorization code invalid") || (message.contains("authorization code") && message.contains("used")) || message.contains("pkce verification failed")
}
