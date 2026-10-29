use std::sync::Arc;
use futures::future::BoxFuture;
use crate::errors::McpError;
use crate::transport_sdk::McpClient;

pub trait McpAuthChallengeHandler: Send + Sync {
    fn reauthorize(&self, scopes: Vec<String>) -> BoxFuture<'_, Result<(), McpError>>;
    fn renew(&self) -> BoxFuture<'_, Result<Arc<McpClient>, McpError>>;
}

/// Parsed Bearer `WWW-Authenticate` challenge (RFC 6750 §3).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AuthChallenge {
    pub scopes: Vec<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
    pub resource_metadata: Option<String>,
}

/// Parses a `Bearer` challenge; non-Bearer schemes and parameterless challenges yield `None`.
pub fn parse_www_authenticate(header: &str) -> Option<AuthChallenge> {
    let header = header.trim_start();
    let scheme_end = header.find(|c: char| c.is_whitespace()).unwrap_or(header.len());
    if !header[..scheme_end].eq_ignore_ascii_case("bearer") {
        return None;
    }
    let params = parse_params(&header[scheme_end..]);
    if params.is_empty() {
        return None;
    }
    let mut challenge = AuthChallenge::default();
    for (name, value) in params {
        match name.to_ascii_lowercase().as_str() {
            "scope" if challenge.scopes.is_empty() => {
                challenge.scopes = value.split(' ').filter(|s| !s.is_empty()).map(str::to_owned).collect();
            }
            "error" if challenge.error.is_none() => challenge.error = Some(value),
            "error_description" if challenge.error_description.is_none() => challenge.error_description = Some(value),
            "resource_metadata" if challenge.resource_metadata.is_none() => challenge.resource_metadata = Some(value),
            _ => {}
        }
    }
    Some(challenge)
}

fn parse_params(input: &str) -> Vec<(String, String)> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        while i < chars.len() && (chars[i].is_whitespace() || chars[i] == ',') {
            i += 1;
        }
        let start = i;
        while i < chars.len() && chars[i] != '=' && chars[i] != ',' && !chars[i].is_whitespace() {
            i += 1;
        }
        let name: String = chars[start..i].iter().collect();
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if i >= chars.len() || chars[i] != '=' {
            continue;
        }
        i += 1;
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < chars.len() && chars[i] == '"' {
            i += 1;
            while i < chars.len() && chars[i] != '"' {
                if chars[i] == '\\' && i + 1 < chars.len() {
                    i += 1;
                }
                value.push(chars[i]);
                i += 1;
            }
            i += 1;
        } else {
            while i < chars.len() && chars[i] != ',' && !chars[i].is_whitespace() {
                value.push(chars[i]);
                i += 1;
            }
        }
        if !name.is_empty() {
            out.push((name, value));
        }
    }
    out
}

/// A step-up is required for a 403 whose challenge says `insufficient_scope` or carries scopes.
pub fn step_up_required(status: u16, www_authenticate: Option<&str>) -> Option<AuthChallenge> {
    if status != 403 {
        return None;
    }
    let challenge = parse_www_authenticate(www_authenticate?)?;
    (challenge.error.as_deref() == Some("insufficient_scope") || !challenge.scopes.is_empty()).then_some(challenge)
}

/// Union of both scope lists, first-seen order, no duplicates.
pub fn merge_scopes(current: &[String], required: &[String]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    for scope in current.iter().chain(required) {
        if !merged.contains(scope) {
            merged.push(scope.clone());
        }
    }
    merged
}
