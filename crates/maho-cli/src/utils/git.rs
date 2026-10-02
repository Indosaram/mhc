pub struct GitSource { pub repo: String, pub host: String, pub path: String, pub reference: Option<String>, pub pinned: bool }
fn unsafe_part(value: &str, allow_slash: bool) -> bool {
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%' && !bytes.get(index + 1..index + 3).is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit)) { return true; }
    }
    let Ok(decoded) = percent_encoding::percent_decode_str(value).decode_utf8() else { return true; };
    [value, decoded.as_ref()].into_iter().any(|value| value.contains(['\0', '\\']) || value.starts_with('/') || (!allow_slash && value.contains('/')) || value.split('/').any(|part| part == ".."))
}
pub fn parse_generic_git_url(source: &str) -> Option<GitSource> {
    let source = source.trim();
    let prefixed = source.starts_with("git:");
    let input = if prefixed { source[4..].trim() } else { source };
    if !prefixed && !["http://", "https://", "ssh://", "git://"].iter().any(|prefix| input.to_lowercase().starts_with(prefix)) { return None; }
    let (mut repo, host, mut path, reference) = if let Some(scp) = input.strip_prefix("git@") {
        let (host, path) = scp.split_once(':')?;
        let (path, reference) = path.split_once('@').filter(|(path, reference)| !path.is_empty() && !reference.is_empty()).map_or((path, None), |(path, reference)| (path, Some(reference.to_owned())));
        (format!("git@{host}:{path}"), host.to_owned(), path.to_owned(), reference)
    } else if input.contains("://") {
        let mut parsed = url::Url::parse(input).ok()?;
        if !matches!(parsed.scheme(), "http" | "https" | "ssh" | "git") { return None; }
        let host = parsed.host_str()?.to_owned();
        let path = parsed.path().trim_start_matches('/').to_owned();
        let split = path.split_once('@').filter(|(path, reference)| !path.is_empty() && !reference.is_empty());
        let (path, reference) = split.map_or((path.as_str(), None), |(path, reference)| (path, Some(reference.to_owned())));
        let path = path.to_owned();
        let repo = if reference.is_some() { parsed.set_path(&format!("/{path}")); parsed.to_string().trim_end_matches('/').to_owned() } else { input.to_owned() };
        (repo, host, path, reference)
    } else {
        let (host, path) = input.split_once('/')?;
        if !host.contains('.') && host != "localhost" { return None; }
        let (path, reference) = path.split_once('@').filter(|(path, reference)| !path.is_empty() && !reference.is_empty()).map_or((path, None), |(path, reference)| (path, Some(reference.to_owned())));
        (format!("https://{host}/{path}"), host.to_owned(), path.to_owned(), reference)
    };
    if path.starts_with('/') { return None; }
    path = path.strip_suffix(".git").unwrap_or(&path).trim_start_matches('/').to_owned();
    if host.is_empty() || path.is_empty() || path.split('/').count() < 2 || unsafe_part(&host, false) || unsafe_part(&path, true) { return None; }
    if repo.is_empty() { repo = input.to_owned(); }
    Some(GitSource { repo, host, path, pinned: reference.as_ref().is_some_and(|reference| !reference.is_empty()), reference })
}
pub fn parse_git_url(source: &str) -> Option<GitSource> {
    if let Ok(url) = url::Url::parse(source.trim().strip_prefix("git:").unwrap_or(source.trim()))
        && url.host_str() == Some("gist.github.com")
        && url.path().trim_matches('/').split('/').count() == 1
    {
        let path = format!("null/{}", url.path().trim_matches('/'));
        if unsafe_part(&path, true) { return None; }
        let reference = url.fragment().filter(|value| !value.is_empty()).map(str::to_owned);
        return Some(GitSource { repo: url.to_string(), host: "gist.github.com".into(), path, pinned: reference.is_some(), reference });
    }
    if let Some(input) = source.trim().strip_prefix("git:") {
        let input = input.trim();
        if !input.contains("://") && !input.starts_with("git@") {
            let (domain, path) = if let Some(path) = input.strip_prefix("github:") { ("github.com", path) }
                else if let Some(path) = input.strip_prefix("gitlab:") { ("gitlab.com", path) }
                else if let Some(path) = input.strip_prefix("bitbucket:") { ("bitbucket.org", path) }
                else if let Some(path) = input.strip_prefix("gist:") { ("gist.github.com", path) }
                else if input.split('/').next().is_some_and(|host| !host.contains('.') && host != "localhost" && !host.contains(':')) { ("github.com", input) }
                else { ("", "") };
            if !domain.is_empty() {
                let (path, reference) = path.split_once('@').filter(|(path, reference)| !path.is_empty() && !reference.is_empty()).map_or((path, None), |(path, reference)| (path, Some(reference)));
                let (path, fragment) = path.split_once('#').map_or((path, None), |(path, fragment)| (path, Some(fragment)));
                let path = if domain == "gist.github.com" && !path.contains('/') { format!("null/{path}") } else { path.to_owned() };
                let mut result = parse_git_url(&format!("https://{domain}/{path}"))?;
                result.repo = format!("https://{}", if reference.is_some() { input.split_once('@')?.0 } else { input });
                result.reference = reference.or(fragment).map(str::to_owned).or(result.reference);
                result.pinned = result.reference.is_some();
                return Some(result);
            }
        }
    }
    let mut source = parse_generic_git_url(source)?;
    let domain = source.host.strip_prefix("www.").unwrap_or(&source.host);
    if matches!(domain, "github.com" | "bitbucket.org" | "gitlab.com" | "gist.github.com" | "git.sr.ht") {
        let (path, fragment) = source.path.split_once('#').map_or((source.path.as_str(), None), |(path, fragment)| (path, Some(fragment)));
        let path = path.split('?').next()?;
        let mut parts: Vec<_> = path.split('/').collect();
        let url_fragment = url::Url::parse(&source.repo).ok().and_then(|url| url.fragment().map(str::to_owned));
        let reference = if domain == "github.com" && parts.get(2) == Some(&"tree") { parts.get(3).copied() } else { fragment.or(url_fragment.as_deref()) };
        if domain == "github.com" && parts.len() > 2 && parts[2] != "tree" { return Some(source); }
        if domain == "github.com" || domain == "bitbucket.org" || domain == "git.sr.ht" { parts.truncate(2); }
        let decoded = parts.iter().map(|part| percent_encoding::percent_decode_str(part).decode_utf8().ok().map(|part| part.into_owned())).collect::<Option<Vec<_>>>()?.join("/");
        if unsafe_part(&decoded, true) { return None; }
        source.reference = reference.filter(|reference| !reference.is_empty()).and_then(|reference| percent_encoding::percent_decode_str(reference).decode_utf8().ok().map(|reference| reference.into_owned())).or(source.reference);
        source.path = decoded.strip_suffix(".git").unwrap_or(&decoded).to_owned();
        source.pinned = source.reference.is_some();
        source.host = domain.to_owned();
    }
    Some(source)
}
