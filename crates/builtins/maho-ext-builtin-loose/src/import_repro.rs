use serde_json::Value;
use std::path::{Path,PathBuf};

#[derive(Debug,PartialEq,Eq)]
pub enum SessionReference { Gist { id:String }, File { path:PathBuf }, Issue {owner:String,repo:String,issue:String} }
pub fn parse_ref(reference:&str,cwd:&Path)->Result<SessionReference,String> {
    if reference.ends_with(".html") || reference.ends_with(".jsonl") {
        let path=Path::new(reference);
        let path=if path.is_absolute(){path.to_owned()}else{cwd.join(path)};
        let mut normalized=PathBuf::new();
        for component in path.components() {match component {std::path::Component::CurDir=>{},std::path::Component::ParentDir=>{normalized.pop();},component=>normalized.push(component.as_os_str())}}
        return Ok(SessionReference::File {path:normalized});
    }
    for pattern in [r"^https://pi\.dev/session/#([0-9a-fA-F]{20,})(?:[/#?].*)?$",r"^https://gist\.github\.com/(?:[^/]+/)?([0-9a-fA-F]{20,})(?:[/#?].*)?$",r"^([0-9a-fA-F]{20,})$"] {
        if let Some(captures)=regex::Regex::new(pattern).expect("pinned reference pattern").captures(reference) {return Ok(SessionReference::Gist {id:captures[1].into()});}
    }
    if let Some(captures)=regex::Regex::new(r"^https://github\.com/([^/]+)/([^/]+)/issues/(\d+)(?:[/#?].*)?$").expect("pinned issue pattern").captures(reference) {
        return Ok(SessionReference::Issue {owner:captures[1].into(),repo:captures[2].into(),issue:captures[3].into()});
    }
    Err(format!("expected a gist ID, gist URL, pi.dev share URL, issue URL, .html file, or .jsonl file: {reference}"))
}

pub fn parse_session_jsonl(raw: &str) -> Result<Value, String> {
    let first_line = raw.split('\n').next().unwrap_or("");
    let header: Value = serde_json::from_str(first_line).map_err(|_| "first line of session file is not valid JSON".to_owned())?;
    if header.get("type").and_then(Value::as_str) != Some("session")
        || header.get("id").and_then(Value::as_str).is_none()
        || header.get("cwd").and_then(Value::as_str).is_none_or(str::is_empty) {
        return Err("session file has no valid session header with a cwd".into());
    }
    Ok(header)
}

pub fn decode_exported_html(html:&str)->Result<(Value,String),String> {
    use base64::{Engine,engine::general_purpose::STANDARD};
    let pattern=regex::Regex::new(r#"<script id="session-data" type="application/json">([^<]+)</script>"#).expect("session script");
    let captures=pattern.captures(html).ok_or("HTML does not contain embedded pi session data")?;
    let bytes=STANDARD.decode(&captures[1]).map_err(|_|"embedded pi session data is not valid JSON")?;
    let data:Value=serde_json::from_slice(&bytes).map_err(|_|"embedded pi session data is not valid JSON")?;
    let header=&data["header"];
    if header["type"]!="session"||!header["id"].is_string()||!header["cwd"].is_string() {return Err("embedded pi session data has no valid session header".into());}
    let entries=data["entries"].as_array().ok_or("embedded pi session data has no entries array")?;
    let mut lines=vec![header.to_string()];lines.extend(entries.iter().map(Value::to_string));
    Ok((header.clone(),format!("{}\n",lines.join("\n"))))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionPlatform { Windows, Unix, Unknown }

pub fn detect_session_platform(cwd: &str) -> SessionPlatform {
    let bytes = cwd.as_bytes();
    let drive = bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && matches!(bytes[2], b'/' | b'\\');
    let msys = bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b'/';
    if drive || msys { SessionPlatform::Windows }
    else if cwd.starts_with('/') { SessionPlatform::Unix }
    else { SessionPlatform::Unknown }
}

pub fn rewrite_session_cwd(raw:&str,source:&str,target:&str)->String {
    let escape=|value:&str| {let encoded=serde_json::to_string(value).expect("string encoding");encoded[1..encoded.len()-1].to_owned()};
    let trimmed=source.trim_end_matches(['/', '\\']);
    let mut variants=vec![trimmed.to_owned()];
    let drive=regex::Regex::new(r"^([A-Za-z]):[\\/](.*)$").expect("drive");
    let msys=regex::Regex::new(r"^/([A-Za-z])/(.*)$").expect("msys");
    if let Some(parts)=drive.captures(trimmed).or_else(||msys.captures(trimmed)) {
        let drive=parts[1].to_ascii_uppercase();
        let rest=regex::Regex::new(r"[\\/]+").expect("separators").replace_all(&parts[2],"/").trim_matches('/').to_owned();
        variants.extend([format!("{drive}:\\{}",rest.replace('/',"\\")),format!("{drive}:/{rest}"),format!("/{}/{rest}",drive.to_ascii_lowercase()),format!("/{drive}/{rest}")]);
    }
    variants.sort_by_key(|value|std::cmp::Reverse(value.len()));variants.dedup();
    let target_encoded=escape(target);
    let mut rewritten=raw.to_owned();
    for variant in variants {if !variant.is_empty()&&variant!=target {rewritten=rewritten.replace(&escape(&variant),&target_encoded);}}
    let name=trimmed.rsplit(['/', '\\']).next().unwrap_or("");
    if regex::Regex::new(r"(?i)^pi-ci-[0-9a-f]{32}$").expect("CI directory").is_match(name) {
        for pattern in [format!(r#"[A-Za-z]:(?:[^"\r\n])*?{}"#,regex::escape(name)),format!(r#"/[A-Za-z]/(?:[^"\r\n])*?{}"#,regex::escape(name))] {
            rewritten=regex::Regex::new(&pattern).expect("CI rewrite").replace_all(&rewritten,regex::NoExpand(&target_encoded)).into_owned();
        }
    }
    rewritten
}
