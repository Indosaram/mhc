use rand::RngCore;
use std::{io, path::{Path, PathBuf}};
use subtle::ConstantTimeEq;
use tokio::io::AsyncWriteExt;

pub enum WebSocketListenerAuth { Off, TokenFile(PathBuf), TokenValue(String) }
pub enum ResolvedWebSocketListenerAuth { Off, Bearer { token: String, path: Option<PathBuf> } }
fn trim_token(token: &str) -> &str { token.trim_matches(|character: char| character.is_whitespace() || character == '\u{feff}') }
pub async fn resolve_websocket_listener_auth(auth: Option<WebSocketListenerAuth>, default_path: Option<&Path>) -> io::Result<ResolvedWebSocketListenerAuth> {
    let (path, managed) = match auth {
        Some(WebSocketListenerAuth::Off) => return Ok(ResolvedWebSocketListenerAuth::Off),
        Some(WebSocketListenerAuth::TokenValue(token)) => return Ok(ResolvedWebSocketListenerAuth::Bearer { token, path:None }),
        Some(WebSocketListenerAuth::TokenFile(path)) => (path, false),
        None => match default_path { Some(path) => (path.to_owned(), true), None => return Ok(ResolvedWebSocketListenerAuth::Off) },
    };
    let existing = match tokio::fs::read_to_string(&path).await {
        Ok(token) => Some(trim_token(&token).to_owned()),
        Err(error) if managed && error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    let token = match existing.filter(|token| !managed || !token.is_empty()) {
        Some(token) => token,
        None => {
            let mut bytes = [0; 32]; rand::rng().fill_bytes(&mut bytes);
            let token = bytes.iter().map(|byte| format!("{byte:02x}")).collect::<String>();
            tokio::fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new("."))).await?;
            let mut options = tokio::fs::OpenOptions::new(); options.write(true).create(true).truncate(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&path).await?; file.write_all(format!("{token}\n").as_bytes()).await?;
            #[cfg(unix)]
            { use std::os::unix::fs::PermissionsExt; tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).await?; }
            token
        },
    };
    if token.is_empty() { return Err(io::Error::other(format!("app-server ws auth token file is empty: {}", path.display()))); }
    Ok(ResolvedWebSocketListenerAuth::Bearer { token, path:Some(path) })
}
pub fn is_websocket_request_authorized(header: Option<&str>, auth: &ResolvedWebSocketListenerAuth) -> bool {
    match auth {
        ResolvedWebSocketListenerAuth::Off => true,
        ResolvedWebSocketListenerAuth::Bearer { token, .. } => {
            if token.is_empty() { return false; }
            let Some(actual) = header.and_then(|header| header.strip_prefix("Bearer ")) else { return false; };
            actual.len() == token.len() && bool::from(actual.as_bytes().ct_eq(token.as_bytes()))
        },
    }
}
