use crate::config_schema::{Auth,AuthMode,McpServerConfig,Transport};
#[derive(Debug,Clone,Copy,PartialEq,Eq)]
pub enum ServerAuthMode {None,Bearer,OAuth}
pub fn resolve_auth_mode(config:&McpServerConfig)->ServerAuthMode {
    match &config.auth {
        Some(Auth::Mode(AuthMode::Oauth))=>ServerAuthMode::OAuth,
        Some(Auth::Disabled(false))=>ServerAuthMode::None,
        Some(Auth::Mode(AuthMode::Bearer))=>ServerAuthMode::Bearer,
        Some(Auth::Disabled(true))|None=>{
            if config.bearer_token_env.is_some(){return ServerAuthMode::Bearer;}
            if config.headers.as_ref().is_some_and(|h|!h.is_empty()){return ServerAuthMode::None;}
            if config.transport==Some(Transport::Http) && config.url.as_ref().is_some_and(|u|!u.is_empty()){ServerAuthMode::OAuth}else{ServerAuthMode::None}
        }
    }
}
pub fn detect_literal_bearer_warnings(server:&str,config:&McpServerConfig)->Result<Vec<String>,regex::Error> {
    use sha2::{Digest,Sha256};
    let matcher=regex::Regex::new(r"(?i)(?:bearer\s+\S|sk-[a-z0-9]|[a-z0-9]{24,})")?;
    let mut warnings=Vec::new();
    for (header,value) in config.headers.iter().flat_map(|h|h.iter()) {
        if value.contains("${"){continue;}
        if matcher.is_match(value){let hash=format!("{:x}",Sha256::digest(value));warnings.push(format!("MCP server {server} header '{header}' appears to contain a literal secret (fp {}); use ${{ENV_VAR}} so the token stays out of the config file.",&hash[..8]));}
    }
    Ok(warnings)
}
