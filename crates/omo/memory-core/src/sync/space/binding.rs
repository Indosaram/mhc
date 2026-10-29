//! Stable project bindings and their private, truncated SHA-256 space identities.
use super::SpaceError;
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use unicode_normalization::UnicodeNormalization;

pub type BindingKey = String;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Remote {
    pub name: String,
    pub url: String,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingKind {
    Named,
    Repo,
    Local,
}

pub fn canonical_remote_url(input: &str) -> Result<String, SpaceError> {
    let text = input.trim();
    let (host, path, port) = if text.contains("://") {
        let parsed = url::Url::parse(text).map_err(|e| SpaceError::Remote(e.to_string()))?;
        let host = parsed.host_str().unwrap_or("").to_owned();
        let port = parsed.port().filter(|port| {
            !matches!(
                (parsed.scheme(), *port),
                ("http", 80) | ("https", 443) | ("ssh", 22) | ("git", 9418)
            )
        });
        (host, parsed.path().to_owned(), port.map(|p| p.to_string()))
    } else {
        let scp = text.split_once(':').filter(|(authority, path)| {
            let host = authority
                .rsplit_once('@')
                .map_or(*authority, |(_, host)| host);
            let user = authority.strip_suffix(host).unwrap_or("").strip_suffix('@');
            !host.is_empty()
                && !host.chars().any(|c| c.is_whitespace() || "@/:".contains(c))
                && user.is_none_or(|u| {
                    !u.is_empty() && !u.chars().any(|c| c.is_whitespace() || "@/:".contains(c))
                })
                && path.chars().next().is_none_or(|c| !c.is_whitespace())
        });
        let (host, path) = match scp {
            Some((authority, path)) => (
                authority.rsplit_once('@').map_or(authority, |(_, h)| h),
                path,
            ),
            None => text.split_once('/').unwrap_or((text, "")),
        };
        (host.to_owned(), path.to_owned(), None)
    };
    let mut path = path.trim_matches('/');
    if path
        .get(path.len().saturating_sub(4)..)
        .is_some_and(|s| s.eq_ignore_ascii_case(".git"))
    {
        path = &path[..path.len() - 4];
    }
    path = path.trim_end_matches('/');
    let authority = match port {
        Some(port) => format!("{}:{port}", host.to_lowercase()),
        None => host.to_lowercase(),
    };
    Ok(if path.is_empty() {
        authority
    } else {
        format!("{authority}/{path}")
    })
}
pub fn named_binding(profile: &str) -> Result<BindingKey, SpaceError> {
    let value: String = profile.trim().nfc().collect();
    if value.is_empty() {
        return Err(SpaceError::EmptyProfile);
    }
    Ok(format!("named:{value}"))
}
pub fn repo_binding(remotes: &[Remote]) -> Option<BindingKey> {
    let mut ordered: Vec<_> = remotes.iter().collect();
    ordered.sort_by(|a, b| {
        (a.name != "origin")
            .cmp(&(b.name != "origin"))
            .then_with(|| a.name.encode_utf16().cmp(b.name.encode_utf16()))
    });
    ordered.into_iter().find_map(|r| {
        canonical_remote_url(&r.url)
            .ok()
            .map(|url| format!("repo:{url}"))
    })
}
fn is_local_hex(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn local_binding(entropy: &str) -> Result<BindingKey, SpaceError> {
    if !is_local_hex(entropy) {
        return Err(SpaceError::Entropy);
    }
    Ok(format!("local:{entropy}"))
}
pub fn mint_local_binding() -> Result<BindingKey, SpaceError> {
    let mut bytes = [0; 16];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| SpaceError::Random)?;
    local_binding(&bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}
pub fn parse_binding(raw: &str) -> Option<(BindingKind, &str)> {
    for (prefix, kind) in [
        ("named:", BindingKind::Named),
        ("repo:", BindingKind::Repo),
        ("local:", BindingKind::Local),
    ] {
        if let Some(value) = raw.strip_prefix(prefix) {
            return (!value.is_empty() && (kind != BindingKind::Local || is_local_hex(value)))
                .then_some((kind, value));
        }
    }
    None
}
pub fn space_id_for_binding(binding: &str) -> String {
    crate::support::sha256::sha256_hex(binding.as_bytes())[..32].to_owned()
}
pub fn synced_agent_dir_name(space_id: &str) -> String {
    format!("synced-{}", space_id.chars().take(8).collect::<String>())
}
