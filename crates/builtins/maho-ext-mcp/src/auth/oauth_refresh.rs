use serde_json::Value;
use super::{token_store::McpStoredAuth,oauth_provider::REFRESH_LEEWAY_MS,oauth_errors::{OAuthFlowError,OAuthFailureKind}};
pub fn is_token_stale(record:Option<&McpStoredAuth>,now:f64)->bool {
    let Some(record)=record else{return true;};
    if record.access_token.as_ref().is_none_or(String::is_empty){return true;}
    record.expires_at.is_some_and(|expires|expires-now<=REFRESH_LEEWAY_MS)
}
pub fn assert_s256_supported(metadata:Option<&Value>,server:&str)->Result<(),Box<OAuthFlowError>> {
    if metadata.and_then(|metadata|metadata.get("code_challenge_methods_supported")).and_then(Value::as_array).is_some_and(|methods|methods.contains(&Value::String("S256".into()))){return Ok(());}
    Err(Box::new(OAuthFlowError::new(OAuthFailureKind::S256Unsupported,format!("MCP server {server} authorization server does not advertise PKCE S256 (code_challenge_methods_supported); refusing to authorize without proof-key protection."))))
}
